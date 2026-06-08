//! Compose-engine fidelity tests — ports of the C++ oracle suites
//! `tests/test_compose.cpp`, `tests/test_chips.cpp`, `tests/test_static_fields.cpp`
//! (compose slots), `tests/test_default_class_footer.cpp` (the pure-compose
//! slot), `tests/test_command_row.cpp`, and `tests/test_overlay_null_rtti.cpp`.
//!
//! The remaining `#[ignore]`'d test depends on a module still skeletoned:
//! `crate::core` type-inference (`typeinfer`, a `todo!()`) plus the `symbols`
//! RTTI walker. It is a faithful port kept for the integrator to un-ignore once
//! typeinfer lands. (`crate::addr` static-field evaluation is now integrated, so
//! those tests run normally.)

use super::{
    array_elem_count_span_for, array_elem_type_span_for, command_row_root_name_span,
    command_row_src_span, compose, compose_default, format_preview, pointer_kind_span_for,
    pointer_target_span_for, ComposeResult, LineGeometry, K_FOLD_COL, K_TREE_INDENT,
};
use crate::core::linemeta::{find_chip, K_COMMAND_ROW_ID};
use crate::core::{ChipKind, LineKind, LineMeta, Node, NodeKind, NodeTree};
use crate::provider::{BufferProvider, NullProvider, Provider};

// ── shared builders ─────────────────────────────────────────────────────────

fn child(parent: u64, kind: NodeKind, offset: i32, name: &str) -> Node {
    Node {
        parent_id: parent,
        kind,
        offset,
        name: name.to_string(),
        ..Node::default()
    }
}

fn lines(r: &ComposeResult) -> Vec<String> {
    r.text.split('\n').map(|s| s.to_string()).collect()
}

fn first_chip(r: &ComposeResult, k: ChipKind) -> Option<&crate::core::LineChip> {
    for lm in &r.meta {
        if let Some(c) = find_chip(lm, k) {
            return Some(c);
        }
    }
    None
}

fn count_chips(r: &ComposeResult, k: ChipKind) -> i32 {
    let mut n = 0;
    for lm in &r.meta {
        for c in &lm.chips {
            if c.kind == k {
                n += 1;
            }
        }
    }
    n
}

/// `QString::mid(start, end-start)` over UTF-16 units, returned as a String.
fn mid(s: &str, start: i32, end: i32) -> String {
    let u: Vec<u16> = s.encode_utf16().collect();
    let start = start.max(0) as usize;
    let end = (end.max(0) as usize).min(u.len());
    if start >= end {
        return String::new();
    }
    String::from_utf16_lossy(&u[start..end])
}

// ═══════════════════════════════════════════════════════════════════════════
// test_compose.cpp — structural tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn basic_struct() {
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Root".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(child(root_id, NodeKind::Hex32, 0, "field_0"));
    tree.add_node(child(root_id, NodeKind::Float, 4, "value"));

    let prov = NullProvider;
    let r = compose_default(&tree, &prov);

    assert_eq!(r.meta.len(), 4);
    assert_eq!(r.meta[0].line_kind, LineKind::CommandRow);
    assert!(!r.meta[1].fold_head);
    assert_eq!(r.meta[1].depth, 1);
    assert!(!r.meta[2].fold_head);
    assert_eq!(r.meta[2].depth, 1);
    assert_eq!(r.meta[1].offset_text, "0000 ");
    assert_eq!(r.meta[2].offset_text, "0004 ");
    assert_eq!(r.meta[3].line_kind, LineKind::Footer);
}

#[test]
fn vec3_single_line() {
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Root".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(child(root_id, NodeKind::Vec3, 0, "pos"));

    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    assert_eq!(r.meta.len(), 3);
    assert!(!r.meta[1].is_continuation);
    assert_eq!(r.meta[1].offset_text, "0000 ");
    assert_eq!(r.meta[1].depth, 1);
    assert_eq!(r.meta[1].node_kind, NodeKind::Vec3);
    assert_eq!(r.meta[2].line_kind, LineKind::Footer);
}

#[test]
fn hex_node_compose() {
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "R".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(child(root_id, NodeKind::Hex8, 0, "pad"));
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    assert_eq!(r.meta.len(), 3);
    assert_eq!(r.meta[1].depth, 1);
    assert_eq!(r.meta[2].line_kind, LineKind::Footer);
}

#[test]
fn null_pointer_marker() {
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "R".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(child(root_id, NodeKind::Pointer64, 0, "ptr"));
    let prov = BufferProvider::new(vec![0u8; 64], "");
    let r = compose_default(&tree, &prov);
    assert_eq!(r.meta.len(), 3);
    assert_eq!(r.meta[1].marker_mask & (1u32 << 2), 0); // M_PTR0
    assert_eq!(r.meta[1].depth, 1);
    assert_eq!(r.meta[2].line_kind, LineKind::Footer);
}

#[test]
fn collapsed_struct() {
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Root".into(),
        collapsed: true,
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(child(root_id, NodeKind::Hex32, 0, "field"));
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    assert_eq!(r.meta.len(), 3);
    assert_eq!(r.meta[1].line_kind, LineKind::Field);
    assert_eq!(r.meta[1].depth, 1);
    assert_eq!(r.meta[2].line_kind, LineKind::Footer);
}

#[test]
fn unreadable_pointer_no_read() {
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "R".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(child(root_id, NodeKind::Pointer64, 0, "ptr"));
    let prov = BufferProvider::new(vec![0u8; 4], "");
    let r = compose_default(&tree, &prov);
    assert_eq!(r.meta.len(), 3);
    assert_eq!(r.meta[1].marker_mask & (1u32 << 4), 0); // M_ERR
    assert_eq!(r.meta[1].marker_mask & (1u32 << 2), 0); // M_PTR0
    assert_eq!(r.meta[1].depth, 1);
    assert_eq!(r.meta[2].line_kind, LineKind::Footer);
}

#[test]
fn fold_levels() {
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Root".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    let ci = tree.add_node(Node {
        collapsed: false,
        ..child(root_id, NodeKind::Struct, 0, "Child")
    });
    let child_id = tree.nodes[ci].id;
    tree.add_node(child(child_id, NodeKind::Hex8, 0, "x"));

    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    assert_eq!(r.meta[1].fold_level, 0x401 | 0x2000);
    assert_eq!(r.meta[1].depth, 1);
    assert!(r.meta[1].fold_head);
    assert_eq!(r.meta[2].fold_level, 0x402);
    assert_eq!(r.meta[2].depth, 2);
}

#[test]
fn nested_struct() {
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Outer".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(child(root_id, NodeKind::UInt32, 0, "flags"));
    let ii = tree.add_node(Node {
        collapsed: false,
        ..child(root_id, NodeKind::Struct, 4, "Inner")
    });
    let inner_id = tree.nodes[ii].id;
    tree.add_node(child(inner_id, NodeKind::UInt16, 0, "x"));
    tree.add_node(child(inner_id, NodeKind::UInt16, 2, "y"));

    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    assert_eq!(r.meta.len(), 7);
    assert_eq!(r.meta[1].line_kind, LineKind::Field);
    assert_eq!(r.meta[1].depth, 1);
    assert_eq!(r.meta[2].line_kind, LineKind::Header);
    assert_eq!(r.meta[2].depth, 1);
    assert!(r.meta[2].fold_head);
    assert_eq!(r.meta[2].fold_level, 0x401 | 0x2000);
    assert_eq!(r.meta[3].depth, 2);
    assert_eq!(r.meta[3].fold_level, 0x402);
    assert_eq!(r.meta[4].depth, 2);
    assert_eq!(r.meta[5].line_kind, LineKind::Footer);
    assert_eq!(r.meta[5].depth, 1);
    assert_eq!(r.meta[6].line_kind, LineKind::Footer);
    assert_eq!(r.meta[6].depth, 0);
}

#[test]
fn pointer_deref_expansion() {
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let mi = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Main".into(),
        ..Node::default()
    });
    let main_id = tree.nodes[mi].id;
    tree.add_node(child(main_id, NodeKind::UInt32, 0, "magic"));
    let ti = tree.add_node(Node {
        offset: 200,
        collapsed: false,
        kind: NodeKind::Struct,
        name: "VTable".into(),
        ..Node::default()
    });
    let tmpl_id = tree.nodes[ti].id;
    tree.add_node(child(tmpl_id, NodeKind::UInt64, 0, "fn_one"));
    tree.add_node(child(tmpl_id, NodeKind::UInt64, 8, "fn_two"));
    tree.add_node(Node {
        ref_id: tmpl_id,
        collapsed: false,
        ..child(main_id, NodeKind::Pointer64, 4, "vtable_ptr")
    });

    let mut data = vec![0u8; 256];
    data[4..12].copy_from_slice(&100u64.to_le_bytes());
    data[100..108].copy_from_slice(&0xDEADBEEFu64.to_le_bytes());
    data[108..116].copy_from_slice(&0xCAFEBABEu64.to_le_bytes());
    let prov = BufferProvider::new(data, "");
    let r = compose_default(&tree, &prov);

    assert_eq!(r.meta.len(), 11);
    assert_eq!(r.meta[1].line_kind, LineKind::Field);
    assert_eq!(r.meta[1].depth, 1);
    assert_eq!(r.meta[2].line_kind, LineKind::Header);
    assert_eq!(r.meta[2].depth, 1);
    assert!(r.meta[2].fold_head);
    assert_eq!(r.meta[2].node_kind, NodeKind::Pointer64);
    assert_eq!(r.meta[3].depth, 2);
    assert_eq!(r.meta[4].depth, 2);
    assert_eq!(r.meta[5].line_kind, LineKind::Footer);
    assert_eq!(r.meta[5].depth, 1);
}

#[test]
fn pointer_deref_null() {
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let mi = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Main".into(),
        ..Node::default()
    });
    let main_id = tree.nodes[mi].id;
    let ti = tree.add_node(Node {
        offset: 200,
        collapsed: false,
        kind: NodeKind::Struct,
        name: "Target".into(),
        ..Node::default()
    });
    let tmpl_id = tree.nodes[ti].id;
    tree.add_node(child(tmpl_id, NodeKind::UInt32, 0, "field"));
    tree.add_node(Node {
        ref_id: tmpl_id,
        collapsed: false,
        ..child(main_id, NodeKind::Pointer64, 0, "ptr")
    });

    let prov = BufferProvider::new(vec![0u8; 256], "");
    let r = compose_default(&tree, &prov);
    assert_eq!(r.meta.len(), 8);
    assert_eq!(r.meta[1].line_kind, LineKind::Header);
    assert_eq!(r.meta[1].depth, 1);
    assert!(r.meta[1].fold_head);
    assert_eq!(r.meta[2].line_kind, LineKind::Field);
    assert_eq!(r.meta[2].depth, 2);
    assert_eq!(r.meta[3].line_kind, LineKind::Footer);
}

#[test]
fn pointer_deref_collapsed() {
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let mi = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Main".into(),
        ..Node::default()
    });
    let main_id = tree.nodes[mi].id;
    let ti = tree.add_node(Node {
        offset: 200,
        collapsed: false,
        kind: NodeKind::Struct,
        name: "Target".into(),
        ..Node::default()
    });
    let tmpl_id = tree.nodes[ti].id;
    tree.add_node(child(tmpl_id, NodeKind::UInt32, 0, "field"));
    tree.add_node(Node {
        ref_id: tmpl_id,
        collapsed: true,
        ..child(main_id, NodeKind::Pointer64, 0, "ptr")
    });

    let mut data = vec![0u8; 256];
    data[0..8].copy_from_slice(&100u64.to_le_bytes());
    let prov = BufferProvider::new(data, "");
    let r = compose_default(&tree, &prov);
    assert_eq!(r.meta.len(), 6);
    assert!(r.meta[1].fold_head);
    assert_eq!(r.meta[1].depth, 1);
}

#[test]
fn pointer_deref_cycle() {
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let mi = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Main".into(),
        ..Node::default()
    });
    let main_id = tree.nodes[mi].id;
    let ti = tree.add_node(Node {
        offset: 200,
        collapsed: false,
        kind: NodeKind::Struct,
        name: "Recursive".into(),
        ..Node::default()
    });
    let tmpl_id = tree.nodes[ti].id;
    tree.add_node(child(tmpl_id, NodeKind::UInt32, 0, "data"));
    tree.add_node(Node {
        ref_id: tmpl_id,
        collapsed: false,
        ..child(tmpl_id, NodeKind::Pointer64, 4, "self")
    });
    tree.add_node(Node {
        ref_id: tmpl_id,
        collapsed: false,
        ..child(main_id, NodeKind::Pointer64, 0, "ptr")
    });

    let mut data = vec![0u8; 256];
    data[0..8].copy_from_slice(&100u64.to_le_bytes());
    data[104..112].copy_from_slice(&100u64.to_le_bytes());
    let prov = BufferProvider::new(data, "");
    let r = compose_default(&tree, &prov);
    assert!(!r.meta.is_empty());
    assert!(r.meta.len() < 100);
    assert!(r.meta[1].fold_head);
    assert_eq!(r.meta[1].line_kind, LineKind::Header);
    assert_eq!(r.meta[2].line_kind, LineKind::Field);
}

#[test]
fn struct_footer_simple() {
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Root".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    let ii = tree.add_node(child(root_id, NodeKind::Struct, 0, "Inner"));
    let inner_id = tree.nodes[ii].id;
    tree.add_node(child(inner_id, NodeKind::UInt32, 0, "a"));

    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    let footer_line = r
        .meta
        .iter()
        .position(|m| m.line_kind == LineKind::Footer)
        .expect("should have a footer");
    let ls = lines(&r);
    assert!(ls[footer_line].contains("};"));
    assert!(!ls[footer_line].contains("sizeof"));
}

#[test]
fn line_meta_has_node_id() {
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Root".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(child(root_id, NodeKind::Hex32, 0, "x"));
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    for (i, lm) in r.meta.iter().enumerate() {
        if lm.line_kind == LineKind::CommandRow {
            assert_eq!(lm.node_id, K_COMMAND_ROW_ID);
            assert_eq!(lm.node_idx, -1);
            continue;
        }
        assert_ne!(lm.node_id, 0, "line {i} has nodeId=0");
        let ni = lm.node_idx;
        assert!(ni >= 0 && (ni as usize) < tree.nodes.len());
        assert_eq!(lm.node_id, tree.nodes[ni as usize].id);
    }
}

// ── arrays ──────────────────────────────────────────────────────────────────

fn make_array_root() -> (NodeTree, u64) {
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Root".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    (tree, root_id)
}

#[test]
fn array_header_format() {
    let (mut tree, root_id) = make_array_root();
    tree.add_node(Node {
        element_kind: NodeKind::Int32,
        array_len: 10,
        collapsed: false,
        ..child(root_id, NodeKind::Array, 0, "data")
    });
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    let header_line = r.meta.iter().position(|m| m.is_array_header).unwrap();
    let lm = &r.meta[header_line];
    assert_eq!(lm.line_kind, LineKind::Header);
    assert!(lm.is_array_header);
    assert_eq!(lm.element_kind, NodeKind::Int32);
    assert_eq!(lm.array_count, 10);
    assert!(lm.fold_head);
    assert!(!lm.fold_collapsed);
    let ls = lines(&r);
    let text = &ls[header_line];
    assert!(text.contains("int32_t[10]"), "{text}");
    assert!(text.contains("data"), "{text}");
    assert!(text.contains('{'), "{text}");
}

#[test]
fn array_header_char_types() {
    let (mut tree, root_id) = make_array_root();
    tree.add_node(Node {
        element_kind: NodeKind::UInt8,
        array_len: 64,
        ..child(root_id, NodeKind::Array, 0, "str")
    });
    tree.add_node(Node {
        element_kind: NodeKind::UInt16,
        array_len: 32,
        ..child(root_id, NodeKind::Array, 64, "wstr")
    });
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    let ls = lines(&r);
    let mut found_char = false;
    let mut found_wchar = false;
    for (i, lm) in r.meta.iter().enumerate() {
        if !lm.is_array_header {
            continue;
        }
        if ls[i].contains("uint8_t[64]") {
            found_char = true;
        }
        if ls[i].contains("uint16_t[32]") {
            found_wchar = true;
        }
    }
    assert!(found_char);
    assert!(found_wchar);
}

#[test]
fn array_spans_clickable() {
    let (mut tree, root_id) = make_array_root();
    tree.add_node(Node {
        element_kind: NodeKind::UInt32,
        array_len: 5,
        ..child(root_id, NodeKind::Array, 0, "numbers")
    });
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    let header_line = r.meta.iter().position(|m| m.is_array_header).unwrap();
    let ls = lines(&r);
    let line_text = &ls[header_line];
    let lm = &r.meta[header_line];

    let type_span = array_elem_type_span_for(lm, line_text);
    assert!(type_span.valid);
    assert!(type_span.start < type_span.end);
    let type_text = mid(line_text, type_span.start, type_span.end);
    assert!(type_text.contains("uint32_t"), "{type_text}");

    let count_span = array_elem_count_span_for(lm, line_text);
    assert!(count_span.valid);
    assert!(count_span.start < count_span.end);
    assert_eq!(mid(line_text, count_span.start, count_span.end), "5");
}

#[test]
fn array_with_struct_children() {
    let (mut tree, root_id) = make_array_root();
    let ai = tree.add_node(Node {
        element_kind: NodeKind::Int32,
        array_len: 2,
        collapsed: false,
        ..child(root_id, NodeKind::Array, 0, "items")
    });
    let arr_id = tree.nodes[ai].id;
    let e0 = tree.add_node(Node {
        collapsed: false,
        ..child(arr_id, NodeKind::Struct, 0, "Item")
    });
    let e0_id = tree.nodes[e0].id;
    tree.add_node(child(e0_id, NodeKind::UInt32, 0, "value"));
    let e1 = tree.add_node(Node {
        collapsed: false,
        ..child(arr_id, NodeKind::Struct, 4, "Item")
    });
    let e1_id = tree.nodes[e1].id;
    tree.add_node(child(e1_id, NodeKind::UInt32, 0, "value"));

    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    assert!(r.meta.len() > 4);
    let mut found0 = false;
    let mut found1 = false;
    let mut field_count = 0;
    for lm in &r.meta {
        if lm.line_kind == LineKind::ArrayElementSeparator {
            if lm.array_element_idx == 0 {
                found0 = true;
            }
            if lm.array_element_idx == 1 {
                found1 = true;
            }
        }
        if lm.line_kind == LineKind::Field && lm.depth >= 2 {
            field_count += 1;
        }
    }
    assert!(found0);
    assert!(found1);
    assert!(field_count >= 2);
}

#[test]
fn array_collapsed_no_children() {
    let (mut tree, root_id) = make_array_root();
    let ai = tree.add_node(Node {
        element_kind: NodeKind::Float,
        array_len: 100,
        collapsed: true,
        ..child(root_id, NodeKind::Array, 0, "data")
    });
    let arr_id = tree.nodes[ai].id;
    tree.add_node(child(arr_id, NodeKind::Float, 0, "elem"));

    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    assert_eq!(r.meta.len(), 3);
    let arr_line = r.meta.iter().position(|m| m.is_array_header).unwrap();
    assert_eq!(arr_line, 1);
    assert!(r.meta[arr_line].fold_collapsed);
    let ls = lines(&r);
    assert!(!ls[arr_line].contains('{'), "{}", ls[arr_line]);
}

#[test]
fn array_count_recompose() {
    let (mut tree, root_id) = make_array_root();
    let ai = tree.add_node(Node {
        element_kind: NodeKind::UInt8,
        array_len: 10,
        ..child(root_id, NodeKind::Array, 0, "buf")
    });
    let prov = NullProvider;

    let r1 = compose_default(&tree, &prov);
    assert!(lines(&r1).iter().any(|l| l.contains("[10]")));

    tree.nodes[ai].array_len = 42;
    let r2 = compose_default(&tree, &prov);
    let ls2 = lines(&r2);
    let mut found42 = false;
    let mut still10 = false;
    for (i, lm) in r2.meta.iter().enumerate() {
        if lm.is_array_header && ls2[i].contains("uint8_t[42]") {
            found42 = true;
        }
        if lm.is_array_header && ls2[i].contains("uint8_t[10]") {
            still10 = true;
        }
    }
    assert!(found42);
    assert!(!still10);

    let header_line = r2.meta.iter().position(|m| m.is_array_header).unwrap();
    let count_span = array_elem_count_span_for(&r2.meta[header_line], &ls2[header_line]);
    assert!(count_span.valid);
    assert_eq!(
        mid(&ls2[header_line], count_span.start, count_span.end),
        "42"
    );
}

#[test]
fn primitive_array_elements() {
    let (mut tree, root_id) = make_array_root();
    tree.add_node(Node {
        element_kind: NodeKind::UInt32,
        array_len: 4,
        collapsed: false,
        ..child(root_id, NodeKind::Array, 0, "values")
    });
    let mut data = vec![0u8; 64];
    data[0..4].copy_from_slice(&0x11u32.to_le_bytes());
    data[4..8].copy_from_slice(&0x22u32.to_le_bytes());
    data[8..12].copy_from_slice(&0x33u32.to_le_bytes());
    data[12..16].copy_from_slice(&0x44u32.to_le_bytes());
    let prov = BufferProvider::new(data, "");
    let r = compose_default(&tree, &prov);
    let ls = lines(&r);
    let header_line = r.meta.iter().position(|m| m.is_array_header).unwrap();
    assert!(ls[header_line].contains("uint32_t[4]"));

    let mut elem_count = 0;
    let mut found0 = false;
    let mut found3 = false;
    for (i, lm) in r.meta.iter().enumerate() {
        if lm.line_kind == LineKind::Field && lm.depth >= 2 {
            elem_count += 1;
            if ls[i].contains("uint32_t[0]") {
                found0 = true;
            }
            if ls[i].contains("uint32_t[3]") {
                found3 = true;
            }
            assert!(lm.is_array_element, "{}", ls[i]);
        }
    }
    assert_eq!(elem_count, 4);
    assert!(found0);
    assert!(found3);

    let has_footer = r.meta[header_line + 1..]
        .iter()
        .any(|lm| lm.line_kind == LineKind::Footer && lm.node_kind == NodeKind::Array);
    assert!(has_footer);
}

#[test]
fn primitive_array_collapsed() {
    let (mut tree, root_id) = make_array_root();
    tree.add_node(Node {
        element_kind: NodeKind::UInt16,
        array_len: 8,
        collapsed: true,
        ..child(root_id, NodeKind::Array, 0, "data")
    });
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    let elem_fields = r
        .meta
        .iter()
        .filter(|m| m.line_kind == LineKind::Field && m.depth >= 2)
        .count();
    assert_eq!(elem_fields, 0);
}

#[test]
fn struct_array_still_uses_children() {
    let (mut tree, root_id) = make_array_root();
    let ai = tree.add_node(Node {
        element_kind: NodeKind::Struct,
        array_len: 1,
        collapsed: false,
        ..child(root_id, NodeKind::Array, 0, "items")
    });
    let arr_id = tree.nodes[ai].id;
    let ei = tree.add_node(Node {
        collapsed: false,
        ..child(arr_id, NodeKind::Struct, 0, "Item")
    });
    let elem_id = tree.nodes[ei].id;
    tree.add_node(child(elem_id, NodeKind::UInt32, 0, "val"));
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    assert!(lines(&r).iter().any(|l| l.contains("val")));
}

// ── pointers ────────────────────────────────────────────────────────────────

#[test]
fn pointer_default_void() {
    let (mut tree, root_id) = make_array_root();
    tree.add_node(child(root_id, NodeKind::Pointer64, 0, "ptr"));
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    let ptr_line = r
        .meta
        .iter()
        .position(|m| m.node_kind == NodeKind::Pointer64 && m.line_kind == LineKind::Field)
        .unwrap();
    let ls = lines(&r);
    assert!(ls[ptr_line].contains("void*"), "{}", ls[ptr_line]);
    assert!(r.meta[ptr_line].pointer_target_name.is_empty());
    assert!(!r.meta[ptr_line].fold_head);
}

#[test]
fn pointer_default_void_32() {
    let (mut tree, root_id) = make_array_root();
    tree.add_node(child(root_id, NodeKind::Pointer32, 0, "ptr32"));
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    assert!(lines(&r).iter().any(|l| l.contains("void*")));
}

/// A base-translating provider: `read(addr)` maps `addr → data[addr - img_base]`
/// (mirrors the C++ `TestPeProvider` / a real PE attach where reads come in as
/// absolute VAs and the OS does the VA→file-offset mapping). Required because
/// the RVA fix would be invisible to a `base_address = 0` test — adding 0 is a
/// no-op, so the broken and fixed paths would render identically.
struct TestPeProvider {
    data: Vec<u8>,
    img_base: u64,
}
impl crate::provider::Provider for TestPeProvider {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        if addr < self.img_base {
            return false;
        }
        let off = (addr - self.img_base) as usize;
        if off + buf.len() > self.data.len() {
            return false;
        }
        buf.copy_from_slice(&self.data[off..off + buf.len()]);
        true
    }
    fn size(&self) -> i32 {
        self.data.len() as i32
    }
    // The default `is_readable` assumes `addr` is a file offset; override so the
    // base-translation maths matches `read()` above.
    fn is_readable(&self, addr: u64, len: i32) -> bool {
        if len < 0 {
            return false;
        }
        if addr < self.img_base {
            return false;
        }
        let off = (addr - self.img_base) as u64;
        off + (len as u64) <= self.data.len() as u64
    }
    fn base(&self) -> u64 {
        self.img_base
    }
    fn kind(&self) -> String {
        "Process".to_string()
    }
}

/// Regression for the broken-at-top-level RVA pointer semantics. Before the fix:
/// `target = recursion-time base parameter + value`. At the top level `base == 0`,
/// so the target collapsed to just `value`, which sent reads to a literal address
/// like 0x78 instead of imageBase+0x78. PE_Headers.rcx looked correct in tree
/// shape but every dereferenced Signature read 0x00000000 because 0x78 was
/// unmapped. Fix: `target = tree.base_address + value` (PE RVA convention).
#[test]
fn top_level_rva_pointer_resolves_against_tree_base_address() {
    const IMAGE_BASE: u64 = 0x1_4000_0000;
    const ELFANEW: u32 = 0x78;
    const PE_SIG: u32 = 0x0000_4550; // 'PE\0\0'

    let mut buf = vec![0u8; 0x400];
    buf[0x3C..0x40].copy_from_slice(&ELFANEW.to_le_bytes());
    buf[0x78..0x7C].copy_from_slice(&PE_SIG.to_le_bytes());

    let prov = TestPeProvider {
        data: buf,
        img_base: IMAGE_BASE,
    };

    let mut tree = NodeTree::new();
    tree.base_address = IMAGE_BASE;

    let di = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "IMAGE_DOS_HEADER".into(),
        name: "dos".into(),
        collapsed: false,
        ..Node::default()
    });
    let dos_id = tree.nodes[di].id;

    let ni = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "IMAGE_NT_HEADERS64".into(),
        name: "nt".into(),
        collapsed: true,
        ..Node::default()
    });
    let nt_id = tree.nodes[ni].id;

    tree.add_node(Node {
        kind: NodeKind::UInt32,
        name: "Signature".into(),
        parent_id: nt_id,
        offset: 0,
        ..Node::default()
    });

    tree.add_node(Node {
        kind: NodeKind::Pointer32,
        name: "e_lfanew".into(),
        parent_id: dos_id,
        offset: 0x3C,
        ref_id: nt_id,
        is_relative: true,
        collapsed: false, // expand the pointer so Signature renders
        ..Node::default()
    });

    let r = compose_default(&tree, &prov);

    // The Signature row's resolved address must be imageBase + e_lfanew, NOT just
    // e_lfanew. Look up the meta entry whose node is the Signature field.
    let expected = IMAGE_BASE + ELFANEW as u64;
    let mut found = false;
    for lm in &r.meta {
        if lm.line_kind != LineKind::Field {
            continue;
        }
        let idx = lm.node_idx;
        if idx < 0 || idx as usize >= tree.nodes.len() {
            continue;
        }
        if tree.nodes[idx as usize].name != "Signature" {
            continue;
        }
        found = true;
        assert_eq!(lm.offset_addr, expected);
        break;
    }
    assert!(
        found,
        "Signature line never appeared in compose output — Pointer32 RVA \
         expansion is silently broken"
    );

    // The bytes read at that target should be the PE magic (would read 0 if we
    // sent the provider 0x78 instead of 0x140000078).
    assert_eq!(prov.read_u32(expected), PE_SIG);

    // The expanded e_lfanew header must surface the raw stored RVA (0x78).
    // Without this a user can't see the value that drove the resolved target.
    // The header line is the one that ends in "{" and contains "e_lfanew".
    let mut found_header = false;
    for line in lines(&r) {
        if !line.contains("e_lfanew") {
            continue;
        }
        if !line.trim_end().ends_with('{') {
            continue;
        }
        found_header = true;
        assert!(
            line.contains("0x78"),
            "expanded e_lfanew header must show the raw stored RVA value '0x78'; \
             got:\n  {line}"
        );
        break;
    }
    assert!(
        found_header,
        "expanded e_lfanew header line ending with '{{' not found"
    );
}

/// Regression: the non-RVA case of the same rendering change. Expanded absolute
/// Pointer64 headers also now show their raw stored value before '{' — keeping
/// the header informative when a pointer's target is known but the user wants to
/// confirm what address it lives at without scanning the next line.
#[test]
fn expanded_absolute_pointer_header_shows_value() {
    const PTR_SLOT: u64 = 0x10;
    const PTR_VALUE: u64 = 0x0000_0ABC_DEF0_1234;

    let mut buf = vec![0u8; 0x80];
    buf[PTR_SLOT as usize..PTR_SLOT as usize + 8].copy_from_slice(&PTR_VALUE.to_le_bytes());
    let prov = BufferProvider::new(buf, "");

    let mut tree = NodeTree::new();
    tree.base_address = 0;

    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Owner".into(),
        name: "owner".into(),
        collapsed: false,
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;

    let ti = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Target".into(),
        name: "target".into(),
        collapsed: true,
        ..Node::default()
    });
    let target_id = tree.nodes[ti].id;

    tree.add_node(Node {
        kind: NodeKind::UInt32,
        name: "x".into(),
        parent_id: target_id,
        offset: 0,
        ..Node::default()
    });

    tree.add_node(Node {
        kind: NodeKind::Pointer64,
        name: "next".into(),
        parent_id: root_id,
        offset: PTR_SLOT as i32,
        ref_id: target_id,
        collapsed: false, // expanded
        ..Node::default()
    });

    let r = compose_default(&tree, &prov);

    // fmt_pointer64 hex output is lower-case; compare case-insensitively.
    let stored = format!("0x{PTR_VALUE:x}");
    let mut found = false;
    for line in lines(&r) {
        if !line.contains("next") {
            continue;
        }
        if !line.trim_end().ends_with('{') {
            continue;
        }
        found = true;
        assert!(
            line.to_lowercase().contains(&stored),
            "expanded absolute Pointer header must show its stored value before \
             '{{'; got:\n  {line}"
        );
        break;
    }
    assert!(found, "expanded 'next' header line not found");
}

#[test]
fn pointer_displays_target_name() {
    let (mut tree, root_id) = make_array_root();
    let ti = tree.add_node(Node {
        offset: 200,
        struct_type_name: "PlayerData".into(),
        kind: NodeKind::Struct,
        name: "PlayerData".into(),
        ..Node::default()
    });
    let target_id = tree.nodes[ti].id;
    tree.add_node(child(target_id, NodeKind::UInt32, 0, "health"));
    tree.add_node(Node {
        ref_id: target_id,
        collapsed: true,
        ..child(root_id, NodeKind::Pointer64, 0, "player")
    });
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    let ptr_line = r
        .meta
        .iter()
        .position(|m| m.node_kind == NodeKind::Pointer64 && m.line_kind == LineKind::Field)
        .unwrap();
    let ls = lines(&r);
    assert!(ls[ptr_line].contains("PlayerData*"), "{}", ls[ptr_line]);
    assert_eq!(r.meta[ptr_line].pointer_target_name, "PlayerData");
    assert!(r.meta[ptr_line].fold_head);
    assert!(r.meta[ptr_line].fold_collapsed);
}

#[test]
fn pointer_target_uses_name() {
    let (mut tree, root_id) = make_array_root();
    let ti = tree.add_node(Node {
        offset: 200,
        kind: NodeKind::Struct,
        name: "MyStruct".into(),
        ..Node::default()
    });
    let target_id = tree.nodes[ti].id;
    tree.add_node(Node {
        ref_id: target_id,
        collapsed: true,
        ..child(root_id, NodeKind::Pointer64, 0, "sptr")
    });
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    assert!(lines(&r).iter().any(|l| l.contains("MyStruct*")));
}

#[test]
fn pointer_spans() {
    let (mut tree, root_id) = make_array_root();
    let ti = tree.add_node(Node {
        offset: 200,
        struct_type_name: "VTable".into(),
        kind: NodeKind::Struct,
        name: "VTable".into(),
        ..Node::default()
    });
    let target_id = tree.nodes[ti].id;
    tree.add_node(Node {
        ref_id: target_id,
        collapsed: true,
        ..child(root_id, NodeKind::Pointer64, 0, "vtbl")
    });
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    let ptr_line = r
        .meta
        .iter()
        .position(|m| m.node_kind == NodeKind::Pointer64 && m.line_kind == LineKind::Field)
        .unwrap();
    let ls = lines(&r);
    let line_text = &ls[ptr_line];
    let lm = &r.meta[ptr_line];
    assert!(!pointer_kind_span_for(lm, line_text).valid);
    let target_span = pointer_target_span_for(lm, line_text);
    assert!(target_span.valid);
    assert_eq!(
        mid(line_text, target_span.start, target_span.end).trim(),
        "VTable"
    );
}

#[test]
fn pointer_void_spans() {
    let (mut tree, root_id) = make_array_root();
    tree.add_node(child(root_id, NodeKind::Pointer64, 0, "vptr"));
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    let ptr_line = r
        .meta
        .iter()
        .position(|m| m.node_kind == NodeKind::Pointer64 && m.line_kind == LineKind::Field)
        .unwrap();
    let ls = lines(&r);
    let line_text = &ls[ptr_line];
    let lm = &r.meta[ptr_line];
    assert!(!pointer_kind_span_for(lm, line_text).valid);
    let target_span = pointer_target_span_for(lm, line_text);
    assert!(target_span.valid);
    assert_eq!(
        mid(line_text, target_span.start, target_span.end).trim(),
        "void"
    );
}

#[test]
fn pointer_to_pointer_chain() {
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Root".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;

    let ci = tree.add_node(Node {
        offset: 300,
        collapsed: false,
        struct_type_name: "InnerData".into(),
        kind: NodeKind::Struct,
        name: "InnerData".into(),
        ..Node::default()
    });
    let struct_c = tree.nodes[ci].id;
    tree.add_node(child(struct_c, NodeKind::UInt64, 0, "payload"));

    let bi = tree.add_node(Node {
        offset: 200,
        collapsed: false,
        struct_type_name: "Wrapper".into(),
        kind: NodeKind::Struct,
        name: "Wrapper".into(),
        ..Node::default()
    });
    let struct_b = tree.nodes[bi].id;
    tree.add_node(child(struct_b, NodeKind::UInt32, 0, "flags"));
    tree.add_node(Node {
        ref_id: struct_c,
        collapsed: false,
        ..child(struct_b, NodeKind::Pointer64, 4, "inner")
    });
    tree.add_node(Node {
        ref_id: struct_b,
        collapsed: false,
        ..child(root_id, NodeKind::Pointer64, 0, "wrapper_ptr")
    });

    let mut data = vec![0u8; 400];
    data[0..8].copy_from_slice(&100u64.to_le_bytes());
    data[104..112].copy_from_slice(&150u64.to_le_bytes());
    let prov = BufferProvider::new(data, "");
    let r = compose_default(&tree, &prov);
    assert!(!r.meta.is_empty());
    assert!(r.meta.len() < 200);
    let ls = lines(&r);
    assert!(ls.iter().any(|l| l.contains("Wrapper*")));
    assert!(ls.iter().any(|l| l.contains("InnerData*")));
    let fold_head_count = r
        .meta
        .iter()
        .filter(|lm| lm.fold_head && lm.node_kind == NodeKind::Pointer64)
        .count();
    assert!(fold_head_count >= 2);
}

#[test]
fn pointer_mutual_cycle() {
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let mi = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Main".into(),
        ..Node::default()
    });
    let main_id = tree.nodes[mi].id;
    tree.add_node(child(main_id, NodeKind::UInt32, 0, "tag"));
    let bi = tree.add_node(Node {
        offset: 200,
        collapsed: false,
        kind: NodeKind::Struct,
        name: "StructB".into(),
        ..Node::default()
    });
    let struct_b = tree.nodes[bi].id;
    tree.add_node(child(struct_b, NodeKind::UInt32, 0, "data"));
    tree.add_node(Node {
        ref_id: struct_b,
        collapsed: false,
        ..child(main_id, NodeKind::Pointer64, 4, "to_b")
    });
    tree.add_node(Node {
        ref_id: main_id,
        collapsed: false,
        ..child(struct_b, NodeKind::Pointer64, 4, "back")
    });

    let mut data = vec![0u8; 300];
    data[4..12].copy_from_slice(&100u64.to_le_bytes());
    data[104..112].copy_from_slice(&50u64.to_le_bytes());
    data[54..62].copy_from_slice(&100u64.to_le_bytes());
    let prov = BufferProvider::new(data, "");
    let r = compose_default(&tree, &prov);
    assert!(!r.meta.is_empty());
    assert!(r.meta.len() < 100);
    let ls = lines(&r);
    assert!(ls.iter().any(|l| l.contains("StructB*")));
    assert!(ls.iter().any(|l| l.contains("Main*")));
}

#[test]
fn all_structs_resolved_as_pointer_targets() {
    let (mut tree, root_id) = make_array_root();
    let names = ["Alpha", "Bravo", "Charlie", "Delta"];
    let mut ids = Vec::new();
    for (i, n) in names.iter().enumerate() {
        let si = tree.add_node(Node {
            offset: 1000 + i as i32 * 100,
            struct_type_name: n.to_string(),
            kind: NodeKind::Struct,
            name: n.to_string(),
            ..Node::default()
        });
        let sid = tree.nodes[si].id;
        ids.push(sid);
        tree.add_node(child(sid, NodeKind::UInt32, 0, "x"));
    }
    for (i, &sid) in ids.iter().enumerate() {
        tree.add_node(Node {
            ref_id: sid,
            collapsed: true,
            ..child(
                root_id,
                NodeKind::Pointer64,
                i as i32 * 8,
                &format!("ptr_{}", names[i].to_lowercase()),
            )
        });
    }
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    let ls = lines(&r);
    for n in names {
        let expected = format!("{n}*");
        assert!(
            ls.iter().any(|l| l.contains(&expected)),
            "missing {expected}"
        );
    }
}

#[test]
fn pointer_refid_to_deleted_struct() {
    let (mut tree, root_id) = make_array_root();
    tree.add_node(Node {
        ref_id: 99999,
        ..child(root_id, NodeKind::Pointer64, 0, "dangling")
    });
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    assert!(lines(&r).iter().any(|l| l.contains("void*")));
}

#[test]
fn pointer_collapsed_no_expansion() {
    let (mut tree, root_id) = make_array_root();
    let ti = tree.add_node(Node {
        offset: 200,
        kind: NodeKind::Struct,
        name: "Heavy".into(),
        ..Node::default()
    });
    let target_id = tree.nodes[ti].id;
    for i in 0..10 {
        tree.add_node(child(target_id, NodeKind::UInt64, i * 8, &format!("f{i}")));
    }
    tree.add_node(Node {
        ref_id: target_id,
        collapsed: true,
        ..child(root_id, NodeKind::Pointer64, 0, "heavy_ptr")
    });
    let mut data = vec![0u8; 300];
    data[0..8].copy_from_slice(&100u64.to_le_bytes());
    let prov = BufferProvider::new(data, "");
    let r = compose_default(&tree, &prov);
    let expanded = r
        .meta
        .iter()
        .filter(|lm| {
            lm.depth >= 2
                && lm.node_idx >= 0
                && tree.nodes[lm.node_idx as usize].parent_id == target_id
        })
        .count();
    assert_eq!(expanded, 0);
}

#[test]
fn pointer_width_computation() {
    let (mut tree, root_id) = make_array_root();
    let ti = tree.add_node(Node {
        offset: 200,
        struct_type_name: "VeryLongStructNameForTesting".into(),
        kind: NodeKind::Struct,
        name: "VeryLongStructNameForTesting".into(),
        ..Node::default()
    });
    let target_id = tree.nodes[ti].id;
    tree.add_node(Node {
        ref_id: target_id,
        collapsed: true,
        ..child(root_id, NodeKind::Pointer64, 0, "lptr")
    });
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    assert!(lines(&r)
        .iter()
        .any(|l| l.contains("VeryLongStructNameForTesting*")));
    assert!(r.layout.type_w >= 29, "typeW={}", r.layout.type_w);
}

// ── command-row / text ───────────────────────────────────────────────────────

#[test]
fn command_row_root_name_span_test() {
    let text = "source\u{25BE}  0x0  struct MyClass {";
    let name_span = command_row_root_name_span(text);
    assert!(name_span.valid);
    assert_eq!(mid(text, name_span.start, name_span.end).trim(), "MyClass");
}

#[test]
fn text_is_non_empty() {
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "TestStruct".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(child(root_id, NodeKind::UInt64, 0, "id"));
    tree.add_node(child(root_id, NodeKind::Pointer64, 8, "next"));
    tree.add_node(Node {
        element_kind: NodeKind::Hex8,
        array_len: 16,
        collapsed: true,
        ..child(root_id, NodeKind::Array, 16, "buf")
    });
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    assert!(!r.text.is_empty());
    assert!(r.meta.len() >= 5);
    let ls = lines(&r);
    assert_eq!(ls.len(), r.meta.len());
    for (i, l) in ls.iter().enumerate() {
        assert!(!l.is_empty(), "line {i} empty");
    }
}

// ── unions ────────────────────────────────────────────────────────────────

#[test]
fn union_header_shows_keyword() {
    let (mut tree, root_id) = make_array_root();
    let ui = tree.add_node(Node {
        class_keyword: "union".into(),
        collapsed: false,
        ..child(root_id, NodeKind::Struct, 0, "u1")
    });
    let u_id = tree.nodes[ui].id;
    tree.add_node(child(u_id, NodeKind::UInt32, 0, "asInt"));
    tree.add_node(child(u_id, NodeKind::Float, 0, "asFloat"));
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    let ls = lines(&r);
    let header_line = r
        .meta
        .iter()
        .position(|m| {
            m.line_kind == LineKind::Header && m.node_kind == NodeKind::Struct && m.depth == 1
        })
        .unwrap();
    assert!(ls[header_line].contains("union"), "{}", ls[header_line]);
    let member_lines: Vec<usize> = r
        .meta
        .iter()
        .enumerate()
        .filter(|(_, m)| m.line_kind == LineKind::Field && m.depth == 2)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(member_lines.len(), 2);
    assert_eq!(
        r.meta[member_lines[0]].offset_text,
        r.meta[member_lines[1]].offset_text
    );
}

#[test]
fn union_collapsed() {
    let (mut tree, root_id) = make_array_root();
    let ui = tree.add_node(Node {
        class_keyword: "union".into(),
        collapsed: true,
        ..child(root_id, NodeKind::Struct, 0, "u1")
    });
    let u_id = tree.nodes[ui].id;
    tree.add_node(child(u_id, NodeKind::UInt64, 0, "val"));
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    let deep = r
        .meta
        .iter()
        .filter(|m| m.line_kind == LineKind::Field && m.depth >= 2)
        .count();
    assert_eq!(deep, 0);
}

// ── enums ───────────────────────────────────────────────────────────────────

#[test]
fn enum_displays_members() {
    let (mut tree, root_id) = make_array_root();
    tree.add_node(Node {
        class_keyword: "enum".into(),
        struct_type_name: "Color".into(),
        collapsed: false,
        enum_members: vec![("Red".into(), 0), ("Green".into(), 1), ("Blue".into(), 2)],
        ..child(root_id, NodeKind::Struct, 0, "Color")
    });
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    assert!(r.text.contains("Red"));
    assert!(r.text.contains("Green"));
    assert!(r.text.contains("Blue"));
    assert!(r.text.contains("= 0"));
    assert!(r.text.contains("= 2"));
    assert!(r.text.contains("Color"));
}

#[test]
fn enum_collapsed() {
    let (mut tree, root_id) = make_array_root();
    tree.add_node(Node {
        class_keyword: "enum".into(),
        struct_type_name: "Flags".into(),
        collapsed: true,
        enum_members: vec![("A".into(), 0), ("B".into(), 1)],
        ..child(root_id, NodeKind::Struct, 0, "Flags")
    });
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    assert!(!r.text.contains("= 0"));
    assert!(!r.text.contains("= 1"));
    assert!(r.text.contains("Flags"));
}

// ── bitfields ─────────────────────────────────────────────────────────────

#[test]
fn bitfield_members() {
    use crate::core::BitfieldMember;
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Test".into(),
        struct_type_name: "Test".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        class_keyword: "bitfield".into(),
        element_kind: NodeKind::Hex32,
        collapsed: false,
        bitfield_members: vec![
            BitfieldMember {
                name: "Valid".into(),
                bit_offset: 0,
                bit_width: 1,
            },
            BitfieldMember {
                name: "Dirty".into(),
                bit_offset: 1,
                bit_width: 1,
            },
            BitfieldMember {
                name: "PageNum".into(),
                bit_offset: 2,
                bit_width: 20,
            },
        ],
        ..child(root_id, NodeKind::Struct, 0, "flags")
    });
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    assert!(r.text.contains("Valid"));
    assert!(r.text.contains("Dirty"));
    assert!(r.text.contains("PageNum"));
    assert!(r.text.contains(": 1 ="));
    assert!(r.text.contains(": 20 ="));
    assert!(r.meta.iter().any(|m| m.is_member_line));
}

#[test]
fn bitfield_members_three() {
    use crate::core::BitfieldMember;
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "S".into(),
        name: "s".into(),
        collapsed: false,
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        class_keyword: "bitfield".into(),
        element_kind: NodeKind::Hex32,
        collapsed: false,
        bitfield_members: vec![
            BitfieldMember {
                name: "active".into(),
                bit_offset: 0,
                bit_width: 1,
            },
            BitfieldMember {
                name: "level".into(),
                bit_offset: 1,
                bit_width: 3,
            },
            BitfieldMember {
                name: "mode".into(),
                bit_offset: 4,
                bit_width: 4,
            },
        ],
        ..child(root_id, NodeKind::Struct, 0, "flags")
    });
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    assert!(r.text.contains("active"));
    assert!(r.text.contains("level"));
    assert!(r.text.contains("mode"));
    assert_eq!(r.meta.iter().filter(|m| m.is_member_line).count(), 3);
}

#[test]
fn primitive_array_element_count_four() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "S".into(),
        name: "s".into(),
        collapsed: false,
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        array_len: 4,
        element_kind: NodeKind::UInt32,
        collapsed: false,
        ..child(root_id, NodeKind::Array, 0, "values")
    });
    let prov = NullProvider;
    let r = compose_default(&tree, &prov);
    assert_eq!(r.meta.iter().filter(|m| m.is_array_element).count(), 4);
}

// ── comments / brace-wrap / tree-lines ────────────────────────────────────

#[test]
fn compose_with_comments() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "S".into(),
        name: "s".into(),
        collapsed: false,
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        comment: "player HP".into(),
        ..child(root_id, NodeKind::Int32, 0, "health")
    });
    let prov = NullProvider;
    let with = compose(
        &tree, &prov, 0, false, false, false, false, true, true, true,
    );
    assert!(with.text.contains("player HP"));
    let without = compose(
        &tree, &prov, 0, false, false, false, false, false, true, true,
    );
    assert!(!without.text.contains("player HP"));
}

#[test]
fn compose_with_brace_wrap() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "S".into(),
        name: "s".into(),
        collapsed: false,
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(child(root_id, NodeKind::Int32, 0, "x"));
    let prov = NullProvider;
    let r = compose(&tree, &prov, 0, false, false, true, false, true, true, true);
    assert!(r.text.split('\n').any(|l| l.trim() == "{"));
}

#[test]
fn tree_lines_depth2() {
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Unnamed".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(child(root_id, NodeKind::Hex64, 0, ""));
    let ii = tree.add_node(Node {
        collapsed: false,
        offset: 200,
        kind: NodeKind::Struct,
        name: "NewClass".into(),
        ..Node::default()
    });
    let inner_id = tree.nodes[ii].id;
    tree.add_node(child(inner_id, NodeKind::Hex64, 0, ""));
    tree.add_node(child(inner_id, NodeKind::Hex64, 8, ""));
    tree.add_node(child(inner_id, NodeKind::Hex64, 16, ""));
    tree.add_node(Node {
        ref_id: inner_id,
        collapsed: false,
        ..child(root_id, NodeKind::Pointer64, 8, "field_0008")
    });
    tree.add_node(child(root_id, NodeKind::Hex64, 16, ""));

    let mut data = vec![0u8; 256];
    data[8..16].copy_from_slice(&100u64.to_le_bytes());
    let prov = BufferProvider::new(data, "");
    let r = compose(&tree, &prov, 0, false, true, false, false, true, true, true);
    let ls = lines(&r);
    let mut found = false;
    for (i, lm) in r.meta.iter().enumerate() {
        if lm.depth == 2 && lm.line_kind != LineKind::Footer {
            let has = ls[i].contains('\u{2502}')
                || ls[i].contains('\u{251C}')
                || ls[i].contains('\u{2514}');
            if has {
                found = true;
            }
            assert!(has, "depth-2 line {i} missing tree chars: {}", ls[i]);
        }
    }
    assert!(found);
}

// ── default class footer (test_default_class_footer.cpp pure-compose slot) ──

#[test]
fn footer_stays_clean_across_multiple_new_tabs() {
    let mut outputs = Vec::new();
    let prov = NullProvider;
    for tab_no in 0..3 {
        let mut tree = NodeTree::new();
        let ri = tree.add_node(Node {
            kind: NodeKind::Struct,
            name: format!("instance{tab_no}"),
            struct_type_name: format!("UnnamedClass{tab_no}"),
            ..Node::default()
        });
        let root_id = tree.nodes[ri].id;
        for i in 0..16 {
            tree.add_node(child(
                root_id,
                NodeKind::Hex64,
                i * 8,
                &format!("field_{:02x}", i * 8),
            ));
        }
        tree.base_address = 0x400000;
        let r = compose_default(&tree, &prov);
        outputs.push(r.text);
    }
    for (tab_no, out) in outputs.iter().enumerate() {
        let ls: Vec<&str> = out.split('\n').collect();
        assert!(ls.len() >= 2);
        let last = ls[ls.len() - 1];
        let second = ls[ls.len() - 2];
        assert!(last.contains("};"), "tab {tab_no} last: {last}");
        assert!(!last.contains("hex64"), "tab {tab_no} last: {last}");
        assert!(!second.contains("};"), "tab {tab_no} 2nd-last: {second}");
        assert!(
            !last.contains(&format!("instance{tab_no}")),
            "tab {tab_no} last: {last}"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// test_command_row.cpp — the compose span helper (commandRowSrcSpan)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn command_row_src_span_select_source() {
    let row = "   source\u{25BE}  0x0";
    let span = command_row_src_span(row);
    assert!(span.valid);
    assert_eq!(mid(row, span.start, span.end), "source");
}

#[test]
fn command_row_src_span_file_provider() {
    let row = "   'dump.bin'\u{25BE}  0x140000000";
    let span = command_row_src_span(row);
    assert!(span.valid);
    assert_eq!(mid(row, span.start, span.end), "'dump.bin'");
}

#[test]
fn command_row_src_span_process_simulated() {
    let row = "   'notepad.exe'\u{25BE}  0x7FF600000000";
    let span = command_row_src_span(row);
    assert!(span.valid);
    assert_eq!(mid(row, span.start, span.end), "'notepad.exe'");
}

// ═══════════════════════════════════════════════════════════════════════════
// test_chips.cpp
// ═══════════════════════════════════════════════════════════════════════════

const K_STRUCT_BASE: u64 = 0x30000;

#[test]
fn enum_chip_fires_and_can_be_suppressed() {
    let mut tree = NodeTree::new();
    tree.base_address = K_STRUCT_BASE;
    let ei = tree.add_node(Node {
        class_keyword: "enum".into(),
        struct_type_name: "Status".into(),
        name: "Status".into(),
        enum_members: vec![
            ("READY".into(), 0),
            ("RUNNING".into(), 1),
            ("DONE".into(), 2),
        ],
        kind: NodeKind::Struct,
        ..Node::default()
    });
    let enum_id = tree.nodes[ei].id;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Holder".into(),
        name: "Holder".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        ref_id: enum_id,
        ..child(root_id, NodeKind::UInt32, 0, "status")
    });

    let mut data = vec![0u8; (K_STRUCT_BASE + 16) as usize];
    data[K_STRUCT_BASE as usize..K_STRUCT_BASE as usize + 4].copy_from_slice(&1u32.to_le_bytes());
    let prov = BufferProvider::new(data, "synthetic");

    let r = compose(
        &tree, &prov, root_id, false, false, false, false, true, true, true,
    );
    let c = first_chip(&r, ChipKind::Enum).expect("enum chip should fire");
    assert!(c.text.contains("RUNNING"), "{}", c.text);
    assert_eq!(c.enum_current_value, 1);
    assert_eq!(c.enum_ref_node_id, enum_id);
    assert!(c.start_col >= 0);
    assert!(c.end_col > c.start_col);

    let r2 = compose(
        &tree, &prov, root_id, false, false, false, false, true, true, false,
    );
    assert_eq!(count_chips(&r2, ChipKind::Enum), 0);
}

#[test]
fn comment_chip_fires_and_can_be_suppressed() {
    let mut tree = NodeTree::new();
    tree.base_address = K_STRUCT_BASE;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Holder".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        comment: "ref count from header".into(),
        ..child(root_id, NodeKind::UInt32, 0, "count")
    });
    let prov = BufferProvider::new(vec![0u8; (K_STRUCT_BASE + 16) as usize], "synthetic");

    let r = compose(
        &tree, &prov, root_id, false, false, false, false, true, true, true,
    );
    let c = first_chip(&r, ChipKind::Comment).expect("comment chip should fire");
    // The chip text carries the literal "// " lead-in so the rendered row shows
    // `  // ref count from header`, matching the C++ displayed string
    // (`compose.cpp:436`: `lineText += "  // " + commentText`).
    assert_eq!(c.text, "// ref count from header");

    let r2 = compose(
        &tree, &prov, root_id, false, false, false, false, false, true, true,
    );
    assert_eq!(count_chips(&r2, ChipKind::Comment), 0);
}

#[test]
fn multiline_comment_stays_on_one_line() {
    let mut tree = NodeTree::new();
    tree.base_address = K_STRUCT_BASE;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "X".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        comment: "first line\nsecond line\nthird".into(),
        ..child(root_id, NodeKind::UInt32, 0, "v")
    });
    let prov = BufferProvider::new(vec![0u8; (K_STRUCT_BASE + 16) as usize], "synthetic");
    let r = compose(
        &tree, &prov, root_id, false, false, false, false, true, true, true,
    );
    let c = first_chip(&r, ChipKind::Comment).unwrap();
    assert!(!c.text.contains('\n'), "{}", c.text);
    assert!(c.text.contains("first line"));
}

// ── Padding-gap synthetic line (`compose.cpp:850-893`) ──
//
// Consecutive unnamed (padding) children accumulate their byte sizes; just
// before the next NAMED field a non-interactive "[+0xN gap]" continuation
// line is emitted (lowercase hex), then the accumulator resets.

#[test]
fn padding_gap_line_emitted_before_named_field() {
    let mut tree = NodeTree::new();
    tree.base_address = K_STRUCT_BASE;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Padded".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    // named `a` (UInt32 @0), unnamed padding (UInt32 @4 = 4 bytes), named `b` @8
    tree.add_node(child(root_id, NodeKind::UInt32, 0, "a"));
    tree.add_node(child(root_id, NodeKind::UInt32, 4, "")); // padding
    tree.add_node(child(root_id, NodeKind::UInt32, 8, "b"));
    let prov = BufferProvider::new(vec![0u8; (K_STRUCT_BASE + 64) as usize], "synthetic");

    let r = compose(
        &tree, &prov, root_id, false, false, false, false, true, true, true,
    );
    // Lowercase hex: 4 bytes → "[+0x4 gap]".
    assert!(
        r.text.contains("[+0x4 gap]"),
        "expected gap marker, got:\n{}",
        r.text
    );
    // The gap line sits before `b` and after `a`.
    let ls = lines(&r);
    let a_line = ls.iter().position(|l| l.contains("a ")).unwrap();
    let gap_line = ls.iter().position(|l| l.contains("[+0x4 gap]")).unwrap();
    let b_line = ls
        .iter()
        .position(|l| l.contains(" b ") || l.ends_with(" b"))
        .unwrap();
    assert!(a_line < gap_line && gap_line < b_line, "{:#?}", ls);
}

#[test]
fn padding_gap_line_uses_lowercase_hex_for_large_gaps() {
    let mut tree = NodeTree::new();
    tree.base_address = K_STRUCT_BASE;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Padded".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    // 16 bytes of padding split across two unnamed UInt64s, then named `b`.
    tree.add_node(child(root_id, NodeKind::UInt32, 0, "a"));
    tree.add_node(child(root_id, NodeKind::UInt64, 8, "")); // pad 8
    tree.add_node(child(root_id, NodeKind::UInt64, 16, "")); // pad 8 (accumulates → 0x10)
    tree.add_node(child(root_id, NodeKind::UInt32, 24, "b"));
    let prov = BufferProvider::new(vec![0u8; (K_STRUCT_BASE + 64) as usize], "synthetic");

    let r = compose(
        &tree, &prov, root_id, false, false, false, false, true, true, true,
    );
    // 0x10, lowercase, NOT "0X10".
    assert!(
        r.text.contains("[+0x10 gap]"),
        "expected accumulated lowercase gap marker, got:\n{}",
        r.text
    );
    // Exactly one gap marker (the two paddings collapse into one line).
    assert_eq!(r.text.matches("gap]").count(), 1);
}

#[test]
fn trailing_padding_emits_no_gap_line() {
    let mut tree = NodeTree::new();
    tree.base_address = K_STRUCT_BASE;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Padded".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    // named `a`, then trailing unnamed padding with NO named field after it.
    tree.add_node(child(root_id, NodeKind::UInt32, 0, "a"));
    tree.add_node(child(root_id, NodeKind::UInt32, 4, "")); // trailing padding
    let prov = BufferProvider::new(vec![0u8; (K_STRUCT_BASE + 64) as usize], "synthetic");

    let r = compose(
        &tree, &prov, root_id, false, false, false, false, true, true, true,
    );
    assert!(
        !r.text.contains("gap]"),
        "trailing padding should not emit a gap line, got:\n{}",
        r.text
    );
}

#[test]
fn padding_gap_line_is_non_interactive_continuation() {
    let mut tree = NodeTree::new();
    tree.base_address = K_STRUCT_BASE;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Padded".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(child(root_id, NodeKind::UInt32, 0, "a"));
    tree.add_node(child(root_id, NodeKind::UInt32, 4, "")); // padding
    tree.add_node(child(root_id, NodeKind::UInt32, 8, "b"));
    let prov = BufferProvider::new(vec![0u8; (K_STRUCT_BASE + 64) as usize], "synthetic");

    let r = compose(
        &tree, &prov, root_id, false, false, false, false, true, true, true,
    );
    let gap_meta = r
        .meta
        .iter()
        .find(|lm| lm.line_kind == LineKind::Continuation && lm.node_idx == -1)
        .expect("gap line should have node_idx == -1 (non-interactive)");
    assert!(gap_meta.is_continuation);
    assert_eq!(gap_meta.node_id, 0);
    // Offset margin points at the gap's start address (after `a`, before `b`).
    assert_eq!(gap_meta.offset_addr, K_STRUCT_BASE + 4);
    // No chips, default markers (the C++ gap line sets no markerMask).
    assert!(gap_meta.chips.is_empty());
    assert_eq!(gap_meta.marker_mask, 0);
}

#[test]
fn no_gap_line_for_array_elements() {
    // Arrays render children as array elements; the gap-ruler tracking is
    // disabled for them (`!childrenAreArrayElements` guard, compose.cpp:857).
    let mut tree = NodeTree::new();
    tree.base_address = K_STRUCT_BASE;
    let ai = tree.add_node(Node {
        kind: NodeKind::Array,
        element_kind: NodeKind::UInt32,
        array_len: 3,
        struct_type_name: "Arr".into(),
        ..Node::default()
    });
    let arr_id = tree.nodes[ai].id;
    // Explicit child nodes with empty names (array elements are unnamed).
    tree.add_node(child(arr_id, NodeKind::UInt32, 0, ""));
    tree.add_node(child(arr_id, NodeKind::UInt32, 4, ""));
    tree.add_node(child(arr_id, NodeKind::UInt32, 8, ""));
    let prov = BufferProvider::new(vec![0u8; (K_STRUCT_BASE + 64) as usize], "synthetic");

    let r = compose(
        &tree, &prov, arr_id, false, false, false, false, true, true, true,
    );
    assert!(
        !r.text.contains("gap]"),
        "array elements must not trigger gap markers, got:\n{}",
        r.text
    );
}

#[test]
fn comment_chip_carries_slashslash_prefix() {
    // The rendered row must literally show `  // <comment>` to match the C++
    // displayed string (`compose.cpp:436`). The chip text is `// <comment>`
    // and `push_chip` prepends the `"  "` separator.
    let mut tree = NodeTree::new();
    tree.base_address = K_STRUCT_BASE;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Holder".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        comment: "IHDR width".into(),
        ..child(root_id, NodeKind::UInt32, 0, "count")
    });
    let prov = BufferProvider::new(vec![0u8; (K_STRUCT_BASE + 16) as usize], "synthetic");

    let r = compose(
        &tree, &prov, root_id, false, false, false, false, true, true, true,
    );
    let c = first_chip(&r, ChipKind::Comment).expect("comment chip should fire");
    assert_eq!(c.text, "// IHDR width");
    // The rendered text shows the C++ "  // " lead-in.
    assert!(
        r.text.contains("  // IHDR width"),
        "row should show `  // IHDR width`, got:\n{}",
        r.text
    );
}

#[test]
fn chip_spans_match_rendered_text() {
    let mut tree = NodeTree::new();
    tree.base_address = K_STRUCT_BASE;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Holder".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        comment: "this is a comment".into(),
        ..child(root_id, NodeKind::UInt32, 0, "v")
    });
    let prov = BufferProvider::new(vec![0u8; (K_STRUCT_BASE + 16) as usize], "synthetic");
    let r = compose(
        &tree, &prov, root_id, false, false, false, false, true, true, true,
    );

    let text_units: Vec<u16> = r.text.encode_utf16().collect();
    let mut checked = false;
    for (i, lm) in r.meta.iter().enumerate() {
        let Some(c) = find_chip(lm, ChipKind::Comment) else {
            continue;
        };
        let line_start = if i < r.line_starts.len() {
            r.line_starts[i]
        } else {
            -1
        };
        let line_end = if i + 1 < r.line_starts.len() {
            r.line_starts[i + 1] - 1
        } else {
            text_units.len() as i32
        };
        assert!(line_start >= 0);
        // Slice in UTF-16 units (document columns are UTF-16 code units).
        let line_units = &text_units[line_start as usize..line_end as usize];
        assert!(
            c.end_col <= line_units.len() as i32,
            "endCol {} > line len {}",
            c.end_col,
            line_units.len()
        );
        let slice = String::from_utf16_lossy(&line_units[c.start_col as usize..c.end_col as usize]);
        assert_eq!(slice, c.text);
        checked = true;
    }
    assert!(checked);
}

// `chipsAppearInDefinedOrder` + `rttiChipFiresAndCanBeSuppressed` depend on the
// RTTI walker (`symbols` feature), not available in this isolated headless
// build. Faithful ports kept for the integrator to enable.
//
// `typeHintChipFiresAsOverlay` only needs `infer_types` (the `typeinfer`
// module, now implemented), so it runs unconditionally — it never reads any
// vtable. (`test_chips.cpp:176-212`.)
#[test]
fn type_hint_chip_fires_as_overlay() {
    let mut tree = NodeTree::new();
    tree.base_address = K_STRUCT_BASE;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Holder".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(child(root_id, NodeKind::Hex64, 0, "payload"));
    let mut data = vec![0u8; (K_STRUCT_BASE + 16) as usize];
    // Plant two int32s side by side — inferTypes treats this as int32×2 with
    // strong confidence.
    data[K_STRUCT_BASE as usize..K_STRUCT_BASE as usize + 4].copy_from_slice(&14i32.to_le_bytes());
    data[K_STRUCT_BASE as usize + 4..K_STRUCT_BASE as usize + 8]
        .copy_from_slice(&20i32.to_le_bytes());
    let prov = BufferProvider::new(data, "synthetic");
    // type_hints=true (7th arg) — the TypeHint chip is gated on it
    // (`compose.cpp:441`, `test_rtti_hint.cpp:313`).
    let r = compose(
        &tree, &prov, root_id, false, false, false, true, true, true, true,
    );
    let c = first_chip(&r, ChipKind::TypeHint).expect("typehint chip");
    assert!(c.start_col >= 0);
    assert!(c.end_col > c.start_col);
    // Chip text is value-preview + bracketed type label, mirroring
    // `lm.typeHint = preview + " [" + typeName + "]"` (`compose.cpp:450-458`).
    assert!(
        c.text.contains('[') && c.text.ends_with(']'),
        "chip text should carry a bracketed type label: {}",
        c.text
    );
    // The preview of two int32 lanes (14, 20) should precede the bracket.
    assert!(
        c.text.contains("14") && c.text.contains("20"),
        "chip text should preview both lanes: {}",
        c.text
    );
    assert!(!c.type_hint_kinds.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════
// test_overlay_null_rtti.cpp — null-vtable CTA chip (no RTTI walker needed)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn null_typed_pointer_emits_name_class_chip() {
    let mut tree = NodeTree::new();
    tree.base_address = 0x1000;
    let ti = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Target".into(),
        name: "t".into(),
        ..Node::default()
    });
    let target_id = tree.nodes[ti].id;
    let hi = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Host".into(),
        name: "host".into(),
        ..Node::default()
    });
    let host_id = tree.nodes[hi].id;
    tree.add_node(Node {
        ref_id: target_id,
        collapsed: true,
        ..child(host_id, NodeKind::Pointer64, 0, "__vptr")
    });
    let prov = BufferProvider::new(vec![0u8; 0x2000], "synthetic");
    let r = compose(
        &tree, &prov, host_id, false, false, false, false, true, true, true,
    );
    let c = first_chip(&r, ChipKind::Rtti).expect("null-vtable chip");
    assert_eq!(c.text, "(Name class\u{2026})");
    assert_eq!(c.rtti_vtable_addr, 0);
    assert!(c.start_col >= 0);
    assert!(c.end_col > c.start_col);
}

#[test]
fn null_void_pointer_emits_name_class_chip() {
    let mut tree = NodeTree::new();
    tree.base_address = 0x1000;
    let hi = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Host".into(),
        name: "h".into(),
        ..Node::default()
    });
    let host_id = tree.nodes[hi].id;
    tree.add_node(child(host_id, NodeKind::Pointer64, 0, "opaque"));
    let prov = BufferProvider::new(vec![0u8; 0x2000], "synthetic");
    let r = compose(
        &tree, &prov, host_id, false, false, false, false, true, true, true,
    );
    let c = first_chip(&r, ChipKind::Rtti).expect("null void-ptr chip");
    assert_eq!(c.text, "(Name class\u{2026})");
    assert_eq!(c.rtti_vtable_addr, 0);
}

#[test]
fn null_hex64_does_not_get_chip() {
    let mut tree = NodeTree::new();
    tree.base_address = 0x1000;
    let hi = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "H".into(),
        name: "h".into(),
        ..Node::default()
    });
    let host_id = tree.nodes[hi].id;
    tree.add_node(child(host_id, NodeKind::Hex64, 0, "data"));
    let prov = BufferProvider::new(vec![0u8; 0x2000], "synthetic");
    let r = compose(
        &tree, &prov, host_id, false, false, false, false, true, true, true,
    );
    assert_eq!(count_chips(&r, ChipKind::Rtti), 0);
}

#[test]
fn show_rtti_off_suppresses_null_chip() {
    let mut tree = NodeTree::new();
    tree.base_address = 0x1000;
    let hi = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "H".into(),
        name: "h".into(),
        ..Node::default()
    });
    let host_id = tree.nodes[hi].id;
    tree.add_node(child(host_id, NodeKind::Pointer64, 0, "p"));
    let prov = BufferProvider::new(vec![0u8; 0x2000], "synthetic");
    let r = compose(
        &tree, &prov, host_id, false, false, false, false, true, false, true,
    );
    assert_eq!(count_chips(&r, ChipKind::Rtti), 0);
}

// ── geometry unit tests ────────────────────────────────────────────────────

#[test]
fn line_geometry_for_field_line() {
    let lm = LineMeta {
        line_kind: LineKind::Field,
        depth: 1,
        effective_type_w: 14,
        effective_name_w: 22,
        ..Default::default()
    };
    let g = LineGeometry::for_line(&lm);
    // depth 1 → indent_width == K_TREE_INDENT.
    assert_eq!(g.prefix_width, K_FOLD_COL);
    assert_eq!(g.indent_width, K_TREE_INDENT);
    assert_eq!(g.type_start(), K_FOLD_COL + K_TREE_INDENT);
    assert_eq!(g.name_start(), K_FOLD_COL + K_TREE_INDENT + 14 + 1);
    assert_eq!(
        g.value_start(),
        K_FOLD_COL + K_TREE_INDENT + 14 + 1 + 22 + 1
    );
    assert_eq!(g.document_column(5), K_FOLD_COL + 5);
}

#[test]
fn line_geometry_command_row_flush_left() {
    let lm = LineMeta {
        line_kind: LineKind::CommandRow,
        ..Default::default()
    };
    assert_eq!(LineGeometry::for_line(&lm).prefix_width, 0);
}

// ── format_preview unit tests (port of `compose.cpp:18-56` formatPreview) ───

#[test]
fn format_preview_single_float() {
    // 1.0f little-endian = 0x3F800000.
    let d: [u8; 4] = [0x00, 0x00, 0x80, 0x3F];
    assert_eq!(format_preview(&d, 4, &[NodeKind::Float]), "1.0000f");
}

#[test]
fn format_preview_single_uint32_hex() {
    // 0x12345678 little-endian.
    let d: [u8; 4] = [0x78, 0x56, 0x34, 0x12];
    assert_eq!(format_preview(&d, 4, &[NodeKind::UInt32]), "0x12345678");
}

#[test]
fn format_preview_single_pointer64_nullptr() {
    let d: [u8; 8] = [0; 8];
    assert_eq!(format_preview(&d, 8, &[NodeKind::Pointer64]), "nullptr");
}

#[test]
fn format_preview_float_x2_lanes() {
    // Lane 0 = -99999+f overflow cap (1e6f), lane 1 = -0.0f.
    let mut d = [0u8; 8];
    d[0..4].copy_from_slice(&(1.0e6f32).to_le_bytes());
    d[4..8].copy_from_slice(&(-0.0f32).to_le_bytes());
    let out = format_preview(&d, 8, &[NodeKind::Float, NodeKind::Float]);
    assert_eq!(out, "99999+f, -0.000f");
}

#[test]
fn format_preview_utf8_printable_only() {
    let d = *b"Hi\x00\x00\x00\x00\x00\x00";
    assert_eq!(format_preview(&d, 8, &[NodeKind::UTF8]), "\"Hi\"");
}

#[test]
fn format_preview_utf8_nonprintable_first_is_empty() {
    let d = [0x01u8, b'A', 0, 0, 0, 0, 0, 0];
    assert_eq!(format_preview(&d, 8, &[NodeKind::UTF8]), "");
}

#[test]
fn format_preview_empty_kinds() {
    let d = [0u8; 4];
    assert_eq!(format_preview(&d, 4, &[]), "");
}

// ═══════════════════════════════════════════════════════════════════════════
// test_rtti_hint.cpp — inline {RTTI: …} hint + vtable RTTI auto-detect
// ═══════════════════════════════════════════════════════════════════════════

const RTTI_IMAGE_BASE: u64 = 0x10000;
const RTTI_STRUCT_BASE: u64 = 0x30000;

// Build the same synthetic MSVC RTTI shape as `test_rtti.cpp`/`walk.rs`: a
// "Foo" class (bases Bar/Baz) whose vtable lives at IMAGE_BASE+0x1000, plus a
// struct-data region at 0x30000. Mirrors `buildAddressSpaceWithRtti`
// (`test_rtti_hint.cpp:38`).
fn build_address_space_with_rtti() -> Vec<u8> {
    let mut rtti = vec![0u8; 0x10000];
    let wu64 = |b: &mut [u8], at: usize, v: u64| b[at..at + 8].copy_from_slice(&v.to_le_bytes());
    let wu32 = |b: &mut [u8], at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let wcstr = |b: &mut [u8], at: usize, s: &str| {
        let bytes = s.as_bytes();
        b[at..at + bytes.len()].copy_from_slice(bytes);
        b[at + bytes.len()] = 0;
    };

    let vtable_rva = 0x1000usize;
    let td_foo = 0x1100usize;
    let td_bar = 0x1200usize;
    let td_baz = 0x1300usize;
    let chd = 0x1400usize;
    let bca = 0x1500usize;
    let bcd_foo = 0x1600usize;
    let bcd_bar = 0x1700usize;
    let bcd_baz = 0x1800usize;
    let col = 0x1900usize;

    wu64(&mut rtti, vtable_rva - 8, RTTI_IMAGE_BASE + col as u64);
    for i in 0..5usize {
        wu64(
            &mut rtti,
            vtable_rva + i * 8,
            RTTI_IMAGE_BASE + 0x100 + i as u64 * 0x10,
        );
    }
    wu64(&mut rtti, vtable_rva + 5 * 8, 0);

    let write_td = |buf: &mut [u8], rva: usize, name: &str| {
        wu64(buf, rva, 0xDEAD_BEEF);
        wu64(buf, rva + 8, 0);
        wcstr(buf, rva + 16, name);
    };
    write_td(&mut rtti, td_foo, ".?AVFoo@@");
    write_td(&mut rtti, td_bar, ".?AVBar@@");
    write_td(&mut rtti, td_baz, ".?AVBaz@@");

    wu32(&mut rtti, chd, 0);
    wu32(&mut rtti, chd + 0x04, 0);
    wu32(&mut rtti, chd + 0x08, 3);
    wu32(&mut rtti, chd + 0x0C, bca as u32);

    wu32(&mut rtti, bca, bcd_foo as u32);
    wu32(&mut rtti, bca + 4, bcd_bar as u32);
    wu32(&mut rtti, bca + 8, bcd_baz as u32);

    wu32(&mut rtti, bcd_foo, td_foo as u32);
    wu32(&mut rtti, bcd_bar, td_bar as u32);
    wu32(&mut rtti, bcd_baz, td_baz as u32);

    wu32(&mut rtti, col + 0x00, 1);
    wu32(&mut rtti, col + 0x04, 0);
    wu32(&mut rtti, col + 0x08, 0);
    wu32(&mut rtti, col + 0x0C, td_foo as u32);
    wu32(&mut rtti, col + 0x10, chd as u32);
    wu32(&mut rtti, col + 0x14, RTTI_IMAGE_BASE as u32);

    // [0 .. IMAGE_BASE) zeros, [IMAGE_BASE ..) RTTI, struct region at 0x30000.
    let mut data = vec![0u8; (RTTI_STRUCT_BASE + 0x1000) as usize];
    data[RTTI_IMAGE_BASE as usize..RTTI_IMAGE_BASE as usize + rtti.len()].copy_from_slice(&rtti);
    data
}

// BufferProvider that additionally reports a single module covering the
// synthetic RTTI region and counts `enumerate_modules` calls so the test can
// assert the per-pass cache fires it at most once
// (`FakeModuleProvider`, `test_rtti_hint.cpp:101`).
struct FakeModuleProvider {
    inner: BufferProvider,
    enum_calls: std::cell::Cell<u32>,
}
impl FakeModuleProvider {
    fn new(data: Vec<u8>) -> Self {
        FakeModuleProvider {
            inner: BufferProvider::new(data, "synthetic"),
            enum_calls: std::cell::Cell::new(0),
        }
    }
}
impl crate::provider::Provider for FakeModuleProvider {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        self.inner.read(addr, buf)
    }
    fn size(&self) -> i32 {
        self.inner.size()
    }
    fn is_readable(&self, addr: u64, len: i32) -> bool {
        self.inner.is_readable(addr, len)
    }
    fn enumerate_modules(&self) -> Vec<crate::provider::ModuleEntry> {
        self.enum_calls.set(self.enum_calls.get() + 1);
        vec![crate::provider::ModuleEntry {
            name: "synthetic.dll".into(),
            full_path: "synthetic.dll".into(),
            base: RTTI_IMAGE_BASE,
            size: 0x10000,
        }]
    }
}

fn tree_with_hex64_fields(base: u64, n: i32) -> NodeTree {
    let mut tree = NodeTree::new();
    tree.base_address = base;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Demo".into(),
        struct_type_name: "Demo".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    for i in 0..n {
        tree.add_node(child(
            root_id,
            NodeKind::Hex64,
            i * 8,
            &format!("field_{i}"),
        ));
    }
    tree
}

// RTTI chips require the `symbols` feature (the walker is a no-op without it),
// so these RTTI tests only run when that feature is enabled.
#[cfg(feature = "symbols")]
#[test]
fn rtti_hint_attaches_when_value_points_at_vtable() {
    let mut data = build_address_space_with_rtti();
    let vtable_va = RTTI_IMAGE_BASE + 0x1000;
    data[RTTI_STRUCT_BASE as usize..RTTI_STRUCT_BASE as usize + 8]
        .copy_from_slice(&vtable_va.to_le_bytes());

    let prov = FakeModuleProvider::new(data);
    let tree = tree_with_hex64_fields(RTTI_STRUCT_BASE, 4);
    let r = compose_default(&tree, &prov);

    // The Hex64 field at offset 0 (absAddr == struct base) must carry an RTTI
    // chip naming the resolved class.
    let mut found = false;
    for lm in &r.meta {
        if lm.line_kind != LineKind::Field || lm.node_kind != NodeKind::Hex64 {
            continue;
        }
        if lm.offset_addr != RTTI_STRUCT_BASE {
            continue;
        }
        let c = find_chip(lm, ChipKind::Rtti).expect("RTTI chip on vtable field");
        assert!(
            c.text.contains("Foo"),
            "RTTI chip should name the class: {}",
            c.text
        );
        assert!(c.text.starts_with("{RTTI:"), "hint format: {}", c.text);
        assert_eq!(c.rtti_vtable_addr, vtable_va);
        found = true;
        break;
    }
    assert!(found, "did not locate the Hex64 field at offset 0");
}

#[test]
fn rtti_no_hint_when_value_outside_any_module() {
    let mut data = build_address_space_with_rtti();
    let junk = 0xCAFE_BABE_DEAD_BEEFu64;
    data[RTTI_STRUCT_BASE as usize..RTTI_STRUCT_BASE as usize + 8]
        .copy_from_slice(&junk.to_le_bytes());
    let prov = FakeModuleProvider::new(data);
    let tree = tree_with_hex64_fields(RTTI_STRUCT_BASE, 1);
    let r = compose_default(&tree, &prov);
    for lm in &r.meta {
        if lm.node_kind == NodeKind::Hex64 {
            assert!(
                find_chip(lm, ChipKind::Rtti).is_none(),
                "value outside any module must not resolve RTTI"
            );
        }
    }
}

#[cfg(feature = "symbols")]
#[test]
fn rtti_modules_enumerated_few_times_not_per_line() {
    // 32 fields all pointing at the same vtable. Without caching,
    // `enumerate_modules` would fire once per candidate (32×) plus once inside
    // each `walk_rtti` success path (>= 64). With the per-pass module cache +
    // rtti_cache it stays O(1): one call from compose's own cache, plus one
    // inside `walk_rtti`'s `find_owning_module` for the single unique walk
    // (whose RttiInfo is then memoized). (`test_rtti_hint.cpp:213-240`.)
    const FIELD_COUNT: i32 = 32;
    let mut data = build_address_space_with_rtti();
    let vtable_va = RTTI_IMAGE_BASE + 0x1000;
    for i in 0..FIELD_COUNT as usize {
        let off = RTTI_STRUCT_BASE as usize + i * 8;
        data[off..off + 8].copy_from_slice(&vtable_va.to_le_bytes());
    }
    let prov = FakeModuleProvider::new(data);
    let tree = tree_with_hex64_fields(RTTI_STRUCT_BASE, FIELD_COUNT);
    let r = compose_default(&tree, &prov);
    // Must be O(1) in field count — assert generously (<= 4) so future tweaks
    // don't trip a brittle exact-match.
    let calls = prov.enum_calls.get();
    assert!(
        calls <= 4,
        "enumerate_modules called {calls}× for {FIELD_COUNT} fields — should be O(1)"
    );
    assert!((calls as i32) < FIELD_COUNT);
    assert_eq!(
        count_chips(&r, ChipKind::Rtti),
        FIELD_COUNT,
        "every vtable field gets an RTTI chip"
    );
}

#[cfg(feature = "symbols")]
#[test]
fn rtti_hint_on_typed_pointer_header() {
    // A typed Pointer64 whose stored value is the vtable address itself routes
    // through compose_node; its merged fold header must carry the RTTI hint.
    let mut data = build_address_space_with_rtti();
    let vtable_va = RTTI_IMAGE_BASE + 0x1000;
    data[RTTI_STRUCT_BASE as usize..RTTI_STRUCT_BASE as usize + 8]
        .copy_from_slice(&vtable_va.to_le_bytes());

    let mut tree = NodeTree::new();
    tree.base_address = RTTI_STRUCT_BASE;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Demo".into(),
        struct_type_name: "Demo".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    // A target struct for the pointer to reference (gives it a typed header).
    let ti = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Target".into(),
        struct_type_name: "Target".into(),
        ..Node::default()
    });
    let target_id = tree.nodes[ti].id;
    let mut ptr = child(root_id, NodeKind::Pointer64, 0, "vptr");
    ptr.ref_id = target_id;
    ptr.collapsed = true; // keep the header on one line
    tree.add_node(ptr);

    let prov = FakeModuleProvider::new(data);
    let r = compose_default(&tree, &prov);

    let c = first_chip(&r, ChipKind::Rtti).expect("RTTI chip on typed-pointer header");
    assert!(
        c.text.contains("Foo"),
        "header RTTI hint names class: {}",
        c.text
    );
    assert_eq!(c.rtti_vtable_addr, vtable_va);
}

#[test]
fn pointer_to_class_fold_footer_has_add_bytes_pills() {
    // A typed pointer-to-class fold footer carries the same add-bytes pills as a
    // struct footer (`+1 +10h +100h +1000h Trim Top`) so the user can grow the
    // pointed-to class definition from the expansion. (A void pointer with no
    // ref_id keeps a plain `}` — covered by the other pointer tests.)
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let mi = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Main".into(),
        ..Node::default()
    });
    let main_id = tree.nodes[mi].id;
    let ti = tree.add_node(Node {
        offset: 200,
        collapsed: false,
        kind: NodeKind::Struct,
        name: "VTable".into(),
        ..Node::default()
    });
    let tmpl_id = tree.nodes[ti].id;
    tree.add_node(child(tmpl_id, NodeKind::UInt64, 0, "fn_one"));
    tree.add_node(Node {
        ref_id: tmpl_id,
        collapsed: false,
        ..child(main_id, NodeKind::Pointer64, 0, "ptr")
    });

    let mut data = vec![0u8; 256];
    data[0..8].copy_from_slice(&100u64.to_le_bytes()); // ptr -> 100 (readable)
    let prov = BufferProvider::new(data, "");
    let r = compose_default(&tree, &prov);

    let fi = r
        .meta
        .iter()
        .position(|lm| lm.line_kind == LineKind::Footer && lm.node_kind == NodeKind::Pointer64)
        .expect("pointer-fold footer present");
    let text = lines(&r)[fi].clone();
    assert!(
        text.contains("+1 +10h +100h +1000h Trim Top"),
        "pointer-to-class footer should carry add-bytes pills, got: {text:?}"
    );
}

// ── On-line chip ordering: enum -> comment -> typeHint -> RTTI ──
//
// C++ `composeLeaf` (`compose.cpp:388-484`) appends, in order: the enum member
// name, the `// comment` annotation, the type-hint, then the RTTI hint. So a
// field carrying BOTH an enum mapping and a comment must render
// `… (NAME)  // comment` — enum chip first, comment chip second — and the chip
// column spans must reflect that order.
#[test]
fn chip_order_enum_then_comment_on_one_line() {
    let mut tree = NodeTree::new();
    tree.base_address = K_STRUCT_BASE;
    let ei = tree.add_node(Node {
        class_keyword: "enum".into(),
        struct_type_name: "Status".into(),
        name: "Status".into(),
        enum_members: vec![
            ("READY".into(), 0),
            ("RUNNING".into(), 1),
            ("DONE".into(), 2),
        ],
        kind: NodeKind::Struct,
        ..Node::default()
    });
    let enum_id = tree.nodes[ei].id;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Holder".into(),
        name: "Holder".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        ref_id: enum_id,
        comment: "state field".into(),
        ..child(root_id, NodeKind::UInt32, 0, "status")
    });

    let mut data = vec![0u8; (K_STRUCT_BASE + 16) as usize];
    data[K_STRUCT_BASE as usize..K_STRUCT_BASE as usize + 4].copy_from_slice(&1u32.to_le_bytes());
    let prov = BufferProvider::new(data, "synthetic");

    let r = compose(
        &tree, &prov, root_id, false, false, false, false, true, true, true,
    );

    let enum_chip = first_chip(&r, ChipKind::Enum).expect("enum chip should fire");
    let comment_chip = first_chip(&r, ChipKind::Comment).expect("comment chip should fire");
    assert!(enum_chip.text.contains("RUNNING"), "{}", enum_chip.text);
    assert_eq!(comment_chip.text, "// state field");
    // Enum precedes comment on the line (start/end cols ordered).
    assert!(
        enum_chip.end_col <= comment_chip.start_col,
        "enum chip [{},{}) must come before comment chip [{},{})",
        enum_chip.start_col,
        enum_chip.end_col,
        comment_chip.start_col,
        comment_chip.end_col
    );

    // The visible trailing text must read `(RUNNING)` then `// state field`.
    let field_line = lines(&r)
        .into_iter()
        .find(|l| l.contains("status"))
        .expect("status field line present");
    let enum_pos = field_line.find("(RUNNING)").expect("enum text present");
    let comment_pos = field_line
        .find("// state field")
        .expect("comment text present");
    assert!(
        enum_pos < comment_pos,
        "rendered order must be enum then comment: {field_line:?}"
    );
}

// ── Comment chip ordered BEFORE RTTI on a Pointer64 (enum->comment->typeHint->RTTI) ──
#[cfg(feature = "symbols")]
#[test]
fn chip_order_comment_before_rtti() {
    let mut data = build_address_space_with_rtti();
    let vtable_va = RTTI_IMAGE_BASE + 0x1000;
    data[RTTI_STRUCT_BASE as usize..RTTI_STRUCT_BASE as usize + 8]
        .copy_from_slice(&vtable_va.to_le_bytes());

    let mut tree = NodeTree::new();
    tree.base_address = RTTI_STRUCT_BASE;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Demo".into(),
        struct_type_name: "Demo".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    // A bare Pointer64 leaf (no ref_id) carrying a comment whose value lands in
    // the module → both a Comment chip and an RTTI chip on the same row. The
    // comment must precede the RTTI hint.
    tree.add_node(Node {
        comment: "vtable ptr".into(),
        ..child(root_id, NodeKind::Pointer64, 0, "vptr")
    });

    let prov = FakeModuleProvider::new(data);
    let r = compose_default(&tree, &prov);

    let comment_chip = first_chip(&r, ChipKind::Comment).expect("comment chip should fire");
    let rtti_chip = first_chip(&r, ChipKind::Rtti).expect("RTTI chip should fire");
    assert_eq!(comment_chip.text, "// vtable ptr");
    assert!(rtti_chip.text.contains("Foo"), "{}", rtti_chip.text);
    assert!(
        comment_chip.end_col <= rtti_chip.start_col,
        "comment chip [{},{}) must come before RTTI chip [{},{})",
        comment_chip.start_col,
        comment_chip.end_col,
        rtti_chip.start_col,
        rtti_chip.end_col
    );
}

// ── Pointer value symbol annotation surfaces in the live editor line ──
//
// `render::read_value` (the live-editor value formatter) must mirror
// `format::read_value` and append `  // <module>!<symbol>` to Pointer32/64 and
// FuncPtr32/64 values via `prov.get_symbol` (`format.cpp:421-475`). Previously
// the live path dropped this suffix entirely.
#[test]
fn pointer_value_symbol_suffix_in_compose_line() {
    // Provider: one symbol at a chosen pointer value, NO modules (so the RTTI
    // detector never fires — isolates the symbol-suffix path).
    struct SymOnlyProvider {
        sym_val: u64,
    }
    impl crate::provider::Provider for SymOnlyProvider {
        fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            // The pointer field sits at struct base (== K_STRUCT_BASE); return
            // the symbol value there, zeros elsewhere.
            let v: u64 = if addr == K_STRUCT_BASE {
                self.sym_val
            } else {
                0
            };
            let bytes = v.to_le_bytes();
            for (i, b) in buf.iter_mut().enumerate() {
                *b = bytes.get(i).copied().unwrap_or(0);
            }
            true
        }
        fn size(&self) -> i32 {
            (K_STRUCT_BASE + 0x1000) as i32
        }
        fn get_symbol(&self, a: u64) -> String {
            if a == self.sym_val {
                "ntdll!RtlUserThreadStart".to_string()
            } else {
                String::new()
            }
        }
    }

    let mut tree = NodeTree::new();
    tree.base_address = K_STRUCT_BASE;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Holder".into(),
        struct_type_name: "Holder".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    // Bare Pointer64 leaf (no ref_id → leaf path, not a typed header).
    tree.add_node(child(root_id, NodeKind::Pointer64, 0, "pfn"));

    // A value clearly outside our (empty) module set so no RTTI chip fires.
    let sym_val = 0x7FF7_1857_0000u64;
    let prov = SymOnlyProvider { sym_val };
    let r = compose(
        &tree, &prov, root_id, false, false, false, false, true, true, true,
    );

    let line = lines(&r)
        .into_iter()
        .find(|l| l.contains("pfn"))
        .expect("pointer field line present");
    assert!(
        line.contains("// ntdll!RtlUserThreadStart"),
        "Pointer64 value must carry the `// module!symbol` suffix, got: {line:?}"
    );
    assert!(line.contains("0x7ff718570000"), "{line:?}");
}

// ── A comment on a typed pointer-to-class header is INVISIBLE in C++ ──
//
// `composeNode`'s typed-pointer header path (`compose.cpp:1213-1257`) attaches
// only the RTTI hint, never a comment chip — so a comment on a pointer-to-class
// node must produce NO Comment chip (it is only visible on leaf fields).
#[test]
fn typed_pointer_header_drops_comment_chip() {
    let mut tree = NodeTree::new();
    tree.base_address = 0;
    let mi = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Main".into(),
        ..Node::default()
    });
    let main_id = tree.nodes[mi].id;
    let ti = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "VTable".into(),
        ..Node::default()
    });
    let tmpl_id = tree.nodes[ti].id;
    tree.add_node(child(tmpl_id, NodeKind::UInt64, 0, "fn_one"));
    // Pointer-to-class node WITH a comment, collapsed so the header is one line.
    tree.add_node(Node {
        ref_id: tmpl_id,
        collapsed: true,
        comment: "should be invisible on the header".into(),
        ..child(main_id, NodeKind::Pointer64, 0, "ptr")
    });

    let mut data = vec![0u8; 256];
    data[0..8].copy_from_slice(&100u64.to_le_bytes()); // ptr -> 100 (readable)
    let prov = BufferProvider::new(data, "");
    let r = compose_default(&tree, &prov);

    assert_eq!(
        count_chips(&r, ChipKind::Comment),
        0,
        "typed pointer-to-class header must not carry a comment chip"
    );
    assert!(
        !r.text.contains("should be invisible"),
        "comment text must not appear on the pointer header:\n{}",
        r.text
    );
}

// ── The LIVE compose path renders floats/doubles via `crate::format` (F1) ──
//
// `mod render` used to carry its OWN copy of the scalar formatters; that copy
// used Rust's built-in round-HALF-TO-EVEN (`format!("{:.*}")`) and a `%g` that
// missed the exponent carry, so it diverged from `fmt::fmtFloat`/`fmtDouble`
// (Qt `QString::number`, round-HALF-AWAY-from-zero, post-rounding `%g` exp).
// The C++ original has exactly ONE render layer (`format.cpp`), so the live
// editor lines MUST equal `fmt::*`. These goldens lock in the previously
// divergent cases plus a spread of normal values to prove no regression.
//
// Goldens (verified against `crate::format`, which ports `format.cpp`):
//   fmtFloat(37428.5)  = "37429.f"   (half-AWAY; half-even gives "37428.f")
//   fmtDouble(999999.5)= "1e+06"     (%g 6-sig carry bumps the exponent)
//   fmtFloat(-0.0)     = "-0.000f"   (signed-zero preserved, leading '-')
//   fmtFloat(3.5)      = "3.5000f"   (normal)
//   fmtDouble(1.5)     = "1.5"       (normal)
#[test]
fn live_compose_float_double_routes_through_format() {
    // f32 37428.5 = [0x80,0x34,0x12,0x47]; f32 -0.0 = [0,0,0,0x80];
    // f32 3.5 = [0,0,0x60,0x40]; f64 999999.5 = [0,0,0,0,0x7F,0x84,0x2E,0x41];
    // f64 1.5 = [0,0,0,0,0,0,0xF8,0x3F].
    let mut tree = NodeTree::new();
    tree.base_address = K_STRUCT_BASE;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Holder".into(),
        name: "Holder".into(),
        ..Node::default()
    });
    let root_id = tree.nodes[ri].id;
    // Layout: f_half_away@0 (4), f_neg_zero@4 (4), f_normal@8 (4),
    //         d_carry@16 (8), d_normal@24 (8).
    tree.add_node(child(root_id, NodeKind::Float, 0, "f_half_away"));
    tree.add_node(child(root_id, NodeKind::Float, 4, "f_neg_zero"));
    tree.add_node(child(root_id, NodeKind::Float, 8, "f_normal"));
    tree.add_node(child(root_id, NodeKind::Double, 16, "d_carry"));
    tree.add_node(child(root_id, NodeKind::Double, 24, "d_normal"));

    let mut data = vec![0u8; (K_STRUCT_BASE + 64) as usize];
    let base = K_STRUCT_BASE as usize;
    data[base..base + 4].copy_from_slice(&37428.5_f32.to_le_bytes());
    data[base + 4..base + 8].copy_from_slice(&(-0.0_f32).to_le_bytes());
    data[base + 8..base + 12].copy_from_slice(&3.5_f32.to_le_bytes());
    data[base + 16..base + 24].copy_from_slice(&999999.5_f64.to_le_bytes());
    data[base + 24..base + 32].copy_from_slice(&1.5_f64.to_le_bytes());
    let prov = BufferProvider::new(data, "synthetic");

    let r = compose_default(&tree, &prov);
    let find = |needle: &str| -> String {
        lines(&r)
            .into_iter()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("line for {needle:?} present:\n{}", r.text))
    };

    let half = find("f_half_away");
    assert!(
        half.contains("37429.f") && !half.contains("37428.f"),
        "half-AWAY rounding (not half-even) on the live path: {half:?}"
    );

    let neg = find("f_neg_zero");
    // Value column is right of the name; the rendered float value itself must
    // carry the leading '-' from signed-zero preservation.
    assert!(
        neg.contains("-0.000f"),
        "negative-zero float preserves its sign: {neg:?}"
    );

    let fnorm = find("f_normal");
    assert!(
        fnorm.contains("3.5000f"),
        "normal float unchanged: {fnorm:?}"
    );

    let dcarry = find("d_carry");
    assert!(
        dcarry.contains("1e+06"),
        "%g exponent carry on the live path: {dcarry:?}"
    );

    let dnorm = find("d_normal");
    // Match the standalone value token (avoid matching e.g. "1.5000"); the
    // value sits at the end of the line after the name column.
    assert!(
        dnorm.split_whitespace().any(|tok| tok == "1.5"),
        "normal double unchanged: {dnorm:?}"
    );
}
