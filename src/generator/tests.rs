//! Port of `tests/test_generator.cpp` (76 QtTest assertions). Each C++ `void
//! testX()` slot maps to a `#[test] fn x()`; every `QVERIFY(r.contains(s))`
//! becomes `assert!(r.contains(s))` and `QVERIFY(!...)` becomes `assert!(!...)`.
//!
//! `testCppNullptrPointerValue` is intentionally NOT ported here — it exercises
//! `rcx::fmt::fmtPointer*` (the `format` subsystem), not the generator.

use super::*;
use crate::core::{Node, NodeKind, NodeTree};

/// `Node::default()` shorthand used in the inline node-construction pattern.
fn d() -> Node {
    Node::default()
}

/// Mirrors `makeSimpleStruct()` (`test_generator.cpp:12-45`).
fn make_simple_struct() -> NodeTree {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Player".into(),
        struct_type_name: "Player".into(),
        parent_id: 0,
        offset: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::Int32,
        name: "health".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::Float,
        name: "speed".into(),
        parent_id: root_id,
        offset: 4,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::UInt64,
        name: "id".into(),
        parent_id: root_id,
        offset: 8,
        ..d()
    });
    tree
}

// ── Basic struct generation ──

#[test]
fn simple_struct() {
    let tree = make_simple_struct();
    let root_id = tree.nodes[0].id;
    let result = render_cpp(&tree, root_id, None, true);

    assert!(result.contains("#pragma once"));
    assert!(result.contains("// sizeof 0x10"));
    assert!(result.contains("struct Player\n{"));
    assert!(result.contains("int32_t health;"));
    assert!(result.contains("float speed;"));
    assert!(result.contains("uint64_t id;"));
    assert!(result.contains("};"));
    assert!(result.contains("// 0x0"));
    assert!(result.contains("// 0x4"));
    assert!(result.contains("// 0x8"));
    assert!(result.contains("static_assert(sizeof(Player) == 0x10"));

    let no_asserts = render_cpp(&tree, root_id, None, false);
    assert!(!no_asserts.contains("static_assert"));
}

#[test]
fn padding_gaps() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "GappyStruct".into(),
        struct_type_name: "GappyStruct".into(),
        parent_id: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::UInt32,
        name: "a".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::UInt32,
        name: "b".into(),
        parent_id: root_id,
        offset: 8,
        ..d()
    });

    let result = render_cpp(&tree, root_id, None, false);
    assert!(result.contains("uint8_t _pad"));
    assert!(result.contains("[0x4]"));
    assert!(result.contains("uint32_t a;"));
    assert!(result.contains("uint32_t b;"));
}

#[test]
fn tail_padding() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "TailPad".into(),
        struct_type_name: "TailPad".into(),
        parent_id: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::UInt8,
        name: "flag".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::UInt8,
        name: "end".into(),
        parent_id: root_id,
        offset: 16,
        ..d()
    });

    let result = render_cpp(&tree, root_id, None, true);
    assert!(result.contains("[0xF]"));
    assert!(result.contains("static_assert(sizeof(TailPad) == 0x11"));
}

#[test]
fn overlap_warning() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "OverlapStruct".into(),
        struct_type_name: "OverlapStruct".into(),
        parent_id: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::UInt64,
        name: "wide".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::UInt32,
        name: "narrow".into(),
        parent_id: root_id,
        offset: 4,
        ..d()
    });

    let result = render_cpp(&tree, root_id, None, false);
    assert!(result.contains("WARNING: overlap"));
}

#[test]
fn union_no_overlap_warning() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "TestUnion".into(),
        struct_type_name: "TestUnion".into(),
        class_keyword: "union".into(),
        parent_id: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::UInt64,
        name: "wide".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::UInt32,
        name: "narrow".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });

    let result = render_cpp(&tree, root_id, None, false);
    assert!(result.contains("union TestUnion\n{"));
    assert!(result.contains("uint64_t wide;"));
    assert!(result.contains("uint32_t narrow;"));
    assert!(!result.contains("WARNING"));
    assert!(!result.contains("_pad"));
}

#[test]
fn nested_struct() {
    let mut tree = NodeTree::new();
    let oi = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Outer".into(),
        struct_type_name: "Outer".into(),
        parent_id: 0,
        ..d()
    });
    let outer_id = tree.nodes[oi].id;
    let ii = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "pos".into(),
        struct_type_name: "Vec2f".into(),
        parent_id: outer_id,
        offset: 0,
        ..d()
    });
    let inner_id = tree.nodes[ii].id;
    tree.add_node(Node {
        kind: NodeKind::Float,
        name: "x".into(),
        parent_id: inner_id,
        offset: 0,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::Float,
        name: "y".into(),
        parent_id: inner_id,
        offset: 4,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::Int32,
        name: "score".into(),
        parent_id: outer_id,
        offset: 8,
        ..d()
    });

    let result = render_cpp(&tree, outer_id, None, true);
    assert!(result.contains("struct Outer\n{"));
    assert!(result.contains("struct Vec2f pos;"));
    assert!(result.contains("int32_t score;"));
    assert!(result.contains("static_assert(sizeof(Outer) == 0xC"));
}

#[test]
fn primitive_array() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "WithArray".into(),
        struct_type_name: "WithArray".into(),
        parent_id: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::Array,
        name: "data".into(),
        parent_id: root_id,
        offset: 0,
        array_len: 16,
        element_kind: NodeKind::UInt32,
        ..d()
    });

    let result = render_cpp(&tree, root_id, None, false);
    assert!(result.contains("uint32_t data[16];"));
}

#[test]
fn pointer_fields() {
    let mut tree = NodeTree::new();
    let ti = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Target".into(),
        struct_type_name: "TargetData".into(),
        parent_id: 0,
        offset: 0x100,
        ..d()
    });
    let target_id = tree.nodes[ti].id;
    tree.add_node(Node {
        kind: NodeKind::UInt32,
        name: "value".into(),
        parent_id: target_id,
        offset: 0,
        ..d()
    });
    let mi = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Main".into(),
        struct_type_name: "MainStruct".into(),
        parent_id: 0,
        ..d()
    });
    let main_id = tree.nodes[mi].id;
    tree.add_node(Node {
        kind: NodeKind::Pointer64,
        name: "pTarget".into(),
        parent_id: main_id,
        offset: 0,
        ref_id: target_id,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::Pointer64,
        name: "pVoid".into(),
        parent_id: main_id,
        offset: 8,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::Pointer32,
        name: "pTarget32".into(),
        parent_id: main_id,
        offset: 16,
        ref_id: target_id,
        ..d()
    });

    let result = render_cpp(&tree, main_id, None, false);
    assert!(result.contains("struct TargetData* pTarget;"));
    assert!(result.contains("void* pVoid;"));
    assert!(result.contains("struct TargetData* pTarget32;"));
}

#[test]
fn vector_types() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Vectors".into(),
        struct_type_name: "Vectors".into(),
        parent_id: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::Vec2,
        name: "pos2d".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::Vec3,
        name: "pos3d".into(),
        parent_id: root_id,
        offset: 8,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::Vec4,
        name: "color".into(),
        parent_id: root_id,
        offset: 20,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::Mat4x4,
        name: "transform".into(),
        parent_id: root_id,
        offset: 36,
        ..d()
    });

    let result = render_cpp(&tree, root_id, None, false);
    assert!(result.contains("float pos2d[2];"));
    assert!(result.contains("float pos3d[3];"));
    assert!(result.contains("float color[4];"));
    assert!(result.contains("float transform[4][4];"));
}

#[test]
fn string_types() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Strings".into(),
        struct_type_name: "Strings".into(),
        parent_id: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::UTF8,
        name: "name".into(),
        parent_id: root_id,
        offset: 0,
        str_len: 64,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::UTF16,
        name: "wname".into(),
        parent_id: root_id,
        offset: 64,
        str_len: 32,
        ..d()
    });

    let result = render_cpp(&tree, root_id, None, false);
    assert!(result.contains("char name[64];"));
    assert!(result.contains("wchar_t wname[32];"));
}

#[test]
fn full_sdk_export() {
    let mut tree = NodeTree::new();
    let ai = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "StructA".into(),
        struct_type_name: "StructA".into(),
        parent_id: 0,
        offset: 0,
        ..d()
    });
    let a_id = tree.nodes[ai].id;
    tree.add_node(Node {
        kind: NodeKind::UInt32,
        name: "valueA".into(),
        parent_id: a_id,
        offset: 0,
        ..d()
    });
    let bi = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "StructB".into(),
        struct_type_name: "StructB".into(),
        parent_id: 0,
        offset: 0x100,
        ..d()
    });
    let b_id = tree.nodes[bi].id;
    tree.add_node(Node {
        kind: NodeKind::UInt64,
        name: "valueB".into(),
        parent_id: b_id,
        offset: 0,
        ..d()
    });

    let result = render_cpp_all(&tree, None, true);
    assert!(result.contains("struct StructA\n{"));
    assert!(result.contains("struct StructB\n{"));
    assert!(result.contains("uint32_t valueA;"));
    assert!(result.contains("uint64_t valueB;"));
    assert!(result.contains("static_assert(sizeof(StructA) == 0x4"));
    assert!(result.contains("static_assert(sizeof(StructB) == 0x8"));
}

#[test]
fn duplicate_type_name_disambiguation() {
    let mut tree = NodeTree::new();
    let mut make_root = |tree: &mut NodeTree, type_name: &str, offset: i32, field_kind: NodeKind| {
        let ri = tree.add_node(Node {
            kind: NodeKind::Struct,
            struct_type_name: type_name.into(),
            parent_id: 0,
            offset,
            ..d()
        });
        let rid = tree.nodes[ri].id;
        tree.add_node(Node {
            kind: field_kind,
            name: "val".into(),
            parent_id: rid,
            offset: 0,
            ..d()
        });
        rid
    };
    make_root(&mut tree, "Shared", 0x000, NodeKind::UInt32);
    make_root(&mut tree, "Shared", 0x100, NodeKind::UInt64);

    let result = render_cpp_all(&tree, None, false);
    assert!(result.contains("struct Shared\n{"));
    assert!(result.contains("struct Shared_v2\n{"));
    assert!(result.contains("uint32_t val;"));
    assert!(result.contains("uint64_t val;"));
}

#[test]
fn null_generator() {
    let tree = make_simple_struct();
    let result = render_null(&tree, tree.nodes[0].id);
    assert!(result.is_empty());
}

#[test]
fn invalid_root_id() {
    let tree = make_simple_struct();
    let result = render_cpp(&tree, 9999, None, false);
    assert!(result.is_empty());
}

#[test]
fn non_struct_root() {
    let mut tree = NodeTree::new();
    tree.add_node(Node {
        kind: NodeKind::UInt32,
        name: "scalar".into(),
        parent_id: 0,
        ..d()
    });
    let result = render_cpp(&tree, tree.nodes[0].id, None, false);
    assert!(result.is_empty());
}

#[test]
fn empty_struct() {
    let mut tree = NodeTree::new();
    tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Empty".into(),
        struct_type_name: "Empty".into(),
        parent_id: 0,
        ..d()
    });
    let result = render_cpp(&tree, tree.nodes[0].id, None, true);
    assert!(result.contains("struct Empty\n{"));
    assert!(result.contains("};"));
    assert!(result.contains("static_assert(sizeof(Empty) == 0x0"));
}

#[test]
fn name_sanitization() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "my struct-name".into(),
        struct_type_name: "my struct-name".into(),
        parent_id: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::UInt32,
        name: "field with spaces".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });

    let result = render_cpp(&tree, root_id, None, false);
    assert!(result.contains("struct my_struct_name\n{"));
    assert!(result.contains("uint32_t field_with_spaces;"));
}

#[test]
fn export_to_file() {
    let tree = make_simple_struct();
    let root_id = tree.nodes[0].id;
    let text = render_cpp(&tree, root_id, None, true);

    // Round-trip through a temp file (mirrors QTemporaryFile in the C++ test).
    let mut path = std::env::temp_dir();
    path.push(format!("rcx_gen_export_{}.h", std::process::id()));
    std::fs::write(&path, text.as_bytes()).unwrap();
    let read_str = String::from_utf8(std::fs::read(&path).unwrap()).unwrap();
    let _ = std::fs::remove_file(&path);

    assert!(read_str.contains("#pragma once"));
    assert!(read_str.contains("struct Player\n{"));
    assert!(read_str.contains("static_assert"));
}

#[test]
fn full_sdk_no_structs() {
    let mut tree = NodeTree::new();
    tree.add_node(Node {
        kind: NodeKind::UInt32,
        name: "scalar".into(),
        parent_id: 0,
        ..d()
    });
    let result = render_cpp_all(&tree, None, false);
    assert!(result.contains("#pragma once"));
    assert!(!result.contains("struct "));
}

#[test]
fn deeply_nested() {
    let mut tree = NodeTree::new();
    let ai = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "A".into(),
        struct_type_name: "TypeA".into(),
        parent_id: 0,
        ..d()
    });
    let a_id = tree.nodes[ai].id;
    let bi = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "b".into(),
        struct_type_name: "TypeB".into(),
        parent_id: a_id,
        offset: 0,
        ..d()
    });
    let b_id = tree.nodes[bi].id;
    let ci = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "c".into(),
        struct_type_name: "TypeC".into(),
        parent_id: b_id,
        offset: 0,
        ..d()
    });
    let c_id = tree.nodes[ci].id;
    tree.add_node(Node {
        kind: NodeKind::UInt8,
        name: "val".into(),
        parent_id: c_id,
        offset: 0,
        ..d()
    });

    let result = render_cpp(&tree, a_id, None, false);
    assert!(result.contains("struct TypeA\n{"));
    assert!(result.contains("struct TypeB b;"));
}

#[test]
fn inline_anonymous_struct() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "_MMPFN".into(),
        struct_type_name: "_MMPFN".into(),
        parent_id: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    let ui = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "".into(),
        struct_type_name: "".into(),
        class_keyword: "union".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    let union_id = tree.nodes[ui].id;
    tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "ListEntry".into(),
        struct_type_name: "_LIST_ENTRY".into(),
        parent_id: union_id,
        offset: 0,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::UInt64,
        name: "Flags".into(),
        parent_id: union_id,
        offset: 0,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::UInt64,
        name: "PfnCount".into(),
        parent_id: root_id,
        offset: 0x10,
        ..d()
    });

    let result = render_cpp(&tree, root_id, None, false);
    assert!(!result.contains("anon_"));
    assert!(result.contains("union\n    {"));
    assert!(result.contains("struct _LIST_ENTRY ListEntry;"));
    assert!(result.contains("uint64_t Flags;"));
    assert!(result.contains("};"));
    assert!(result.contains("uint64_t PfnCount;"));
}

#[test]
fn opaque_type_no_stub() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Container".into(),
        struct_type_name: "Container".into(),
        parent_id: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "entry".into(),
        struct_type_name: "_LIST_ENTRY".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });

    let result = render_cpp(&tree, root_id, None, false);
    assert!(result.contains("struct _LIST_ENTRY entry;"));
    assert!(!result.contains("struct _LIST_ENTRY\n{"));
    assert!(!result.contains("uint8_t _pad"));
}

// ── Static field tests ──

#[test]
fn static_field_not_in_struct_body() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "MyStruct".into(),
        struct_type_name: "MyStruct".into(),
        parent_id: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::UInt32,
        name: "e_lfanew".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "nt_hdr".into(),
        struct_type_name: "IMAGE_NT_HEADERS".into(),
        parent_id: root_id,
        offset: 0,
        is_static: true,
        offset_expr: "base + e_lfanew".into(),
        ..d()
    });

    let result = render_cpp(&tree, root_id, None, false);
    assert!(
        !result.contains("IMAGE_NT_HEADERS nt_hdr;"),
        "Static field should not be in struct body:\n{}",
        result
    );
    assert!(result.contains("// static:"));
    assert!(result.contains("nt_hdr"));
    assert!(result.contains("base + e_lfanew"));
}

#[test]
fn static_field_comment_format() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Test".into(),
        struct_type_name: "Test".into(),
        parent_id: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::UInt64,
        name: "base_field".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::Hex64,
        name: "ptr".into(),
        parent_id: root_id,
        offset: 0,
        is_static: true,
        offset_expr: "base + 0xFF".into(),
        ..d()
    });

    let result = render_cpp(&tree, root_id, None, false);
    assert!(result.contains("uint64_t base_field;"));
    assert!(result.contains("// static:"));
    assert!(result.contains("@ base + 0xFF"));
}

#[test]
fn struct_size_unchanged_by_static_field() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Small".into(),
        struct_type_name: "Small".into(),
        parent_id: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::UInt32,
        name: "x".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "big_static".into(),
        parent_id: root_id,
        offset: 0,
        is_static: true,
        offset_expr: "base".into(),
        ..d()
    });

    let result = render_cpp(&tree, root_id, None, true);
    assert!(
        result.contains("sizeof(Small) == 0x4"),
        "Expected sizeof(Small) == 0x4:\n{}",
        result
    );
}

// ── Rust backend ──

#[test]
fn rust_simple_struct() {
    let tree = make_simple_struct();
    let root_id = tree.nodes[0].id;
    let result = render_rust(&tree, root_id, None, true);

    assert!(result.contains("// Generated by Reclass 2027"));
    assert!(result.contains("#[repr(C)]"));
    assert!(result.contains("pub struct Player {"));
    assert!(result.contains("pub health: i32,"));
    assert!(result.contains("pub speed: f32,"));
    assert!(result.contains("pub id: u64,"));
    assert!(result.contains("// 0x0"));
    assert!(result.contains("// 0x4"));
    assert!(result.contains("// 0x8"));
    assert!(result.contains("core::mem::size_of::<Player>() == 0x10"));

    let no_asserts = render_rust(&tree, root_id, None, false);
    assert!(!no_asserts.contains("size_of"));
}

#[test]
fn rust_padding() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Padded".into(),
        struct_type_name: "Padded".into(),
        parent_id: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::UInt32,
        name: "a".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::UInt32,
        name: "b".into(),
        parent_id: root_id,
        offset: 8,
        ..d()
    });

    let result = render_rust(&tree, root_id, None, false);
    assert!(result.contains("pub _pad"));
    assert!(result.contains("[u8; 0x4]"));
}

#[test]
fn rust_pointers() {
    let mut tree = NodeTree::new();
    let ti = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Target".into(),
        struct_type_name: "Target".into(),
        parent_id: 0,
        offset: 0x100,
        ..d()
    });
    let target_id = tree.nodes[ti].id;
    tree.add_node(Node {
        kind: NodeKind::UInt32,
        name: "val".into(),
        parent_id: target_id,
        offset: 0,
        ..d()
    });
    let mi = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "PtrTest".into(),
        struct_type_name: "PtrTest".into(),
        parent_id: 0,
        ..d()
    });
    let main_id = tree.nodes[mi].id;
    tree.add_node(Node {
        kind: NodeKind::Pointer64,
        name: "typed".into(),
        parent_id: main_id,
        offset: 0,
        ref_id: target_id,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::Pointer64,
        name: "untyped".into(),
        parent_id: main_id,
        offset: 8,
        ..d()
    });

    let result = render_rust(&tree, main_id, None, false);
    assert!(result.contains("pub typed: *mut Target,"));
    assert!(result.contains("pub untyped: *mut core::ffi::c_void,"));
}

#[test]
fn rust_vectors() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Vecs".into(),
        struct_type_name: "Vecs".into(),
        parent_id: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::Vec2,
        name: "pos".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::Vec4,
        name: "color".into(),
        parent_id: root_id,
        offset: 8,
        ..d()
    });

    let result = render_rust(&tree, root_id, None, false);
    assert!(result.contains("pub pos: [f32; 2],"));
    assert!(result.contains("pub color: [f32; 4],"));
}

#[test]
fn rust_func_ptr() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "FP".into(),
        struct_type_name: "FP".into(),
        parent_id: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::FuncPtr64,
        name: "callback".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });

    let result = render_rust(&tree, root_id, None, false);
    assert!(result.contains("pub callback: Option<unsafe extern \"C\" fn()>,"));
}

#[test]
fn rust_all() {
    let tree = make_simple_struct();
    let result = render_rust_all(&tree, None, true);
    assert!(result.contains("#[repr(C)]"));
    assert!(result.contains("pub struct Player {"));
    assert!(result.contains("core::mem::size_of::<Player>()"));
}

// ── #define backend ──

#[test]
fn define_simple_struct() {
    let tree = make_simple_struct();
    let root_id = tree.nodes[0].id;
    let result = render_defines(&tree, root_id);

    assert!(result.contains("#pragma once"));
    assert!(result.contains("// Player"));
    assert!(result.contains("#define Player_health 0x0"));
    assert!(result.contains("#define Player_speed 0x4"));
    assert!(result.contains("#define Player_id 0x8"));
}

#[test]
fn define_skips_hex() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "HexTest".into(),
        struct_type_name: "HexTest".into(),
        parent_id: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::Hex32,
        name: "padding".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::UInt32,
        name: "real_field".into(),
        parent_id: root_id,
        offset: 4,
        ..d()
    });

    let result = render_defines(&tree, root_id);
    assert!(!result.contains("padding"));
    assert!(result.contains("#define HexTest_real_field 0x4"));
}

#[test]
fn define_all() {
    let tree = make_simple_struct();
    let result = render_defines_all(&tree);
    assert!(result.contains("#pragma once"));
    assert!(result.contains("#define Player_health 0x0"));
}

// ── Format dispatch ──

#[test]
fn code_format_dispatch() {
    let tree = make_simple_struct();
    let root_id = tree.nodes[0].id;

    let cpp = render_code(CodeFormat::CppHeader, &tree, root_id, None, false);
    assert!(cpp.contains("struct Player"));

    let rust = render_code(CodeFormat::RustStruct, &tree, root_id, None, false);
    assert!(rust.contains("pub struct Player"));

    let defs = render_code(CodeFormat::DefineOffsets, &tree, root_id, None, false);
    assert!(defs.contains("#define Player_health"));
}

#[test]
fn code_format_all_dispatch() {
    let tree = make_simple_struct();

    let cpp = render_code_all(CodeFormat::CppHeader, &tree, None, false);
    assert!(cpp.contains("struct Player"));

    let rust = render_code_all(CodeFormat::RustStruct, &tree, None, false);
    assert!(rust.contains("pub struct Player"));

    let defs = render_code_all(CodeFormat::DefineOffsets, &tree, None, false);
    assert!(defs.contains("#define Player_health"));
}

// ── Scope tests ──

#[test]
fn tree_scope_includes_referenced_types() {
    let mut tree = NodeTree::new();
    let ti = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Target".into(),
        struct_type_name: "Target".into(),
        parent_id: 0,
        offset: 0x100,
        ..d()
    });
    let target_id = tree.nodes[ti].id;
    tree.add_node(Node {
        kind: NodeKind::UInt32,
        name: "val".into(),
        parent_id: target_id,
        offset: 0,
        ..d()
    });
    let mi = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Main".into(),
        struct_type_name: "Main".into(),
        parent_id: 0,
        ..d()
    });
    let main_id = tree.nodes[mi].id;
    tree.add_node(Node {
        kind: NodeKind::Pointer64,
        name: "pTarget".into(),
        parent_id: main_id,
        offset: 0,
        ref_id: target_id,
        ..d()
    });

    let current = render_cpp(&tree, main_id, None, false);
    assert!(current.contains("struct Main\n{"));
    assert!(!current.contains("struct Target\n{"));

    let with_deps = render_cpp_tree(&tree, main_id, None, false);
    assert!(with_deps.contains("struct Main\n{"));
    assert!(with_deps.contains("struct Target\n{"));

    let rust_deps = render_rust_tree(&tree, main_id, None, false);
    assert!(rust_deps.contains("pub struct Main {"));
    assert!(rust_deps.contains("pub struct Target {"));

    let def_deps = render_defines_tree(&tree, main_id);
    assert!(def_deps.contains("#define Main_pTarget"));
    assert!(def_deps.contains("#define Target_val"));
}

#[test]
fn tree_scope_dispatch() {
    let mut tree = NodeTree::new();
    let ai = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "A".into(),
        struct_type_name: "A".into(),
        parent_id: 0,
        ..d()
    });
    let a_id = tree.nodes[ai].id;
    tree.add_node(Node {
        kind: NodeKind::UInt32,
        name: "x".into(),
        parent_id: a_id,
        offset: 0,
        ..d()
    });

    let cpp = render_code_tree(CodeFormat::CppHeader, &tree, a_id, None, false);
    assert!(cpp.contains("struct A"));

    let rust = render_code_tree(CodeFormat::RustStruct, &tree, a_id, None, false);
    assert!(rust.contains("pub struct A"));

    let defs = render_code_tree(CodeFormat::DefineOffsets, &tree, a_id, None, false);
    assert!(defs.contains("#define A_x"));

    let cs = render_code_tree(CodeFormat::CSharpStruct, &tree, a_id, None, false);
    assert!(cs.contains("public unsafe struct A"));

    let py = render_code_tree(CodeFormat::PythonCtypes, &tree, a_id, None, false);
    assert!(py.contains("class A(ctypes.Structure)"));
}

// ── C# backend ──

#[test]
fn csharp_simple_struct() {
    let tree = make_simple_struct();
    let root_id = tree.nodes[0].id;
    let result = render_csharp(&tree, root_id, None, false);

    assert!(result.contains("using System.Runtime.InteropServices;"));
    assert!(result.contains("[StructLayout(LayoutKind.Explicit, Size = 0x10)]"));
    assert!(result.contains("public unsafe struct Player"));
    assert!(result.contains("[FieldOffset(0x0)] public int health;"));
    assert!(result.contains("[FieldOffset(0x4)] public float speed;"));
    assert!(result.contains("[FieldOffset(0x8)] public ulong id;"));
}

#[test]
fn csharp_pointers() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Foo".into(),
        struct_type_name: "Foo".into(),
        parent_id: 0,
        offset: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::Pointer64,
        name: "ptr".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });

    let result = render_csharp(&tree, root_id, None, false);
    assert!(result.contains("IntPtr ptr"));
}

#[test]
fn csharp_all() {
    let tree = make_simple_struct();
    let result = render_csharp_all(&tree, None, false);
    assert!(result.contains("public unsafe struct Player"));
    assert!(result.contains("[StructLayout("));
}

#[test]
fn csharp_enum() {
    let mut tree = NodeTree::new();
    tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Color".into(),
        struct_type_name: "Color".into(),
        class_keyword: "enum".into(),
        parent_id: 0,
        offset: 0,
        enum_members: vec![("Red".into(), 0), ("Green".into(), 1), ("Blue".into(), 2)],
        ..d()
    });

    let result = render_csharp_all(&tree, None, false);
    assert!(result.contains("public enum Color : long"));
    assert!(result.contains("Red = 0"));
    assert!(result.contains("Green = 1"));
    assert!(result.contains("Blue = 2"));
}

#[test]
fn csharp_vectors() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Xform".into(),
        struct_type_name: "Xform".into(),
        parent_id: 0,
        offset: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::Vec3,
        name: "position".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });

    let result = render_csharp(&tree, root_id, None, false);
    assert!(result.contains("public fixed float position[3]"));
}

// ── Python backend ──

#[test]
fn python_simple_struct() {
    let tree = make_simple_struct();
    let root_id = tree.nodes[0].id;
    let result = render_python(&tree, root_id);

    assert!(result.contains("import ctypes"));
    assert!(result.contains("class Player(ctypes.Structure)"));
    assert!(result.contains("_fields_ = ["));
    assert!(result.contains("(\"health\", ctypes.c_int32)"));
    assert!(result.contains("(\"speed\", ctypes.c_float)"));
    assert!(result.contains("(\"id\", ctypes.c_uint64)"));
}

#[test]
fn python_pointers() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Bar".into(),
        struct_type_name: "Bar".into(),
        parent_id: 0,
        offset: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::Pointer64,
        name: "ptr".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });

    let result = render_python(&tree, root_id);
    assert!(result.contains("(\"ptr\", ctypes.c_void_p)"));
}

#[test]
fn python_typed_pointers() {
    let mut tree = NodeTree::new();
    let ti = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Target".into(),
        struct_type_name: "Target".into(),
        parent_id: 0,
        offset: 0,
        ..d()
    });
    let target_id = tree.nodes[ti].id;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Holder".into(),
        struct_type_name: "Holder".into(),
        parent_id: 0,
        offset: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::Pointer64,
        name: "ref".into(),
        parent_id: root_id,
        offset: 0,
        ref_id: target_id,
        ..d()
    });

    let result = render_python(&tree, root_id);
    assert!(result.contains("ctypes.POINTER(Target)"));
}

#[test]
fn python_all() {
    let tree = make_simple_struct();
    let result = render_python_all(&tree);
    assert!(result.contains("class Player(ctypes.Structure)"));
}

#[test]
fn python_enum() {
    let mut tree = NodeTree::new();
    tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Status".into(),
        struct_type_name: "Status".into(),
        class_keyword: "enum".into(),
        parent_id: 0,
        offset: 0,
        enum_members: vec![("Active".into(), 1), ("Inactive".into(), 0)],
        ..d()
    });

    let result = render_python_all(&tree);
    assert!(result.contains("class Status:"));
    assert!(result.contains("Active = 1"));
    assert!(result.contains("Inactive = 0"));
}

#[test]
fn python_vectors() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Pos".into(),
        struct_type_name: "Pos".into(),
        parent_id: 0,
        offset: 0,
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::Vec4,
        name: "color".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });

    let result = render_python(&tree, root_id);
    assert!(result.contains("(\"color\", ctypes.c_float * 4)"));
}

#[test]
fn csharp_dispatch() {
    let tree = make_simple_struct();
    let root_id = tree.nodes[0].id;
    let result = render_code(CodeFormat::CSharpStruct, &tree, root_id, None, false);
    assert!(result.contains("[StructLayout("));
}

#[test]
fn python_dispatch() {
    let tree = make_simple_struct();
    let root_id = tree.nodes[0].id;
    let result = render_code(CodeFormat::PythonCtypes, &tree, root_id, None, false);
    assert!(result.contains("ctypes.Structure"));
}

// ── Hex128, enum, union, pointer, bitfield tests ──

#[test]
fn hex128_cpp_output() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Big".into(),
        name: "big".into(),
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::Hex128,
        name: "bigfield".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    let cpp = render_cpp(&tree, root_id, None, false);
    assert!(cpp.contains("cstdint"));
    assert!(cpp.contains("uint8_t") || cpp.contains("0x10"));
}

#[test]
fn hex128_rust_output() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Big".into(),
        name: "big".into(),
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::Hex128,
        name: "bigfield".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    let rs = render_rust(&tree, root_id, None, false);
    assert!(rs.contains("u8") || rs.contains("0x10"));
}

#[test]
fn enum_cpp_output() {
    let mut tree = NodeTree::new();
    tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Colors".into(),
        name: "colors".into(),
        class_keyword: "enum".into(),
        enum_members: vec![("Red".into(), 0), ("Green".into(), 1)],
        ..d()
    });
    let cpp = render_cpp(&tree, tree.nodes[0].id, None, false);
    assert!(cpp.contains("enum Colors"));
    assert!(cpp.contains("Red = 0"));
    assert!(cpp.contains("Green = 1"));
}

#[test]
fn union_cpp_output() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "MyUnion".into(),
        name: "u".into(),
        class_keyword: "union".into(),
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::Int32,
        name: "i".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    tree.add_node(Node {
        kind: NodeKind::Float,
        name: "f".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    let cpp = render_cpp(&tree, root_id, None, false);
    assert!(cpp.contains("union MyUnion"));
}

#[test]
fn python_union_output() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "MyUnion".into(),
        name: "u".into(),
        class_keyword: "union".into(),
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::Int32,
        name: "i".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    let py = render_python(&tree, root_id);
    assert!(py.contains("ctypes.Union"));
}

#[test]
fn pointer_field_cpp() {
    let mut tree = NodeTree::new();
    let ti = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Target".into(),
        name: "t".into(),
        ..d()
    });
    let target_id = tree.nodes[ti].id;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "HasPtr".into(),
        name: "hp".into(),
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::Pointer64,
        name: "target_ptr".into(),
        parent_id: root_id,
        offset: 0,
        ref_id: target_id,
        ..d()
    });
    let cpp = render_cpp(&tree, root_id, None, false);
    assert!(cpp.contains("struct Target* target_ptr"));
}

#[test]
fn csharp_struct_layout_size() {
    let tree = make_simple_struct();
    let cs = render_csharp(&tree, tree.nodes[0].id, None, false);
    assert!(cs.contains("[StructLayout("));
    assert!(cs.contains("FieldOffset"));
}

#[test]
fn align_comments_no_markers() {
    let mut tree = NodeTree::new();
    tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Empty".into(),
        name: "e".into(),
        ..d()
    });
    let cpp = render_cpp(&tree, tree.nodes[0].id, None, false);
    assert!(!cpp.is_empty());
    assert!(cpp.contains("Empty"));
}

#[test]
fn forward_declaration_for_pointer_target() {
    let mut tree = NodeTree::new();
    let bi = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "TargetB".into(),
        name: "b".into(),
        parent_id: 0,
        ..d()
    });
    let b_id = tree.nodes[bi].id;
    tree.add_node(Node {
        kind: NodeKind::Int32,
        name: "val".into(),
        parent_id: b_id,
        offset: 0,
        ..d()
    });
    let ai = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "StructA".into(),
        name: "a".into(),
        parent_id: 0,
        ..d()
    });
    let a_id = tree.nodes[ai].id;
    tree.add_node(Node {
        kind: NodeKind::Pointer64,
        name: "ptr_to_b".into(),
        parent_id: a_id,
        offset: 0,
        ref_id: b_id,
        ..d()
    });

    let cpp = render_cpp(&tree, a_id, None, false);
    assert!(
        cpp.contains("struct TargetB;"),
        "Missing forward declaration. Output:\n{}",
        cpp
    );
    assert!(cpp.contains("struct TargetB* ptr_to_b"));
}

#[test]
fn cpp_includes_cstdint() {
    let tree = make_simple_struct();
    let cpp = render_cpp(&tree, tree.nodes[0].id, None, false);
    assert!(cpp.contains("#include <cstdint>"));
}

#[test]
fn rust_allow_dead_code() {
    let tree = make_simple_struct();
    let rs = render_rust(&tree, tree.nodes[0].id, None, false);
    assert!(rs.contains("#[allow(dead_code)]"));
}

#[test]
fn csharp_nullable_disable() {
    let tree = make_simple_struct();
    let cs = render_csharp(&tree, tree.nodes[0].id, None, false);
    assert!(cs.contains("#nullable disable"));
}

#[test]
fn defines_output() {
    let tree = make_simple_struct();
    let root_id = tree.nodes[0].id;
    let def = render_defines(&tree, root_id);
    assert!(def.contains("#pragma once"));
    assert!(def.contains("Player_health"));
    assert!(def.contains("Player_speed"));
    assert!(def.contains("0x0") || def.contains("0x00"));
}

#[test]
fn python_func_ptr_cfunctype() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "VTable".into(),
        name: "vt".into(),
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::FuncPtr64,
        name: "func".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    let py = render_python(&tree, root_id);
    assert!(py.contains("CFUNCTYPE"), "Expected CFUNCTYPE. Got:\n{}", py);
}

#[test]
fn rust_pointer_field() {
    let mut tree = NodeTree::new();
    let ti = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Target".into(),
        name: "t".into(),
        ..d()
    });
    let target_id = tree.nodes[ti].id;
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "HasPtr".into(),
        name: "hp".into(),
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::Pointer64,
        name: "ptr".into(),
        parent_id: root_id,
        offset: 0,
        ref_id: target_id,
        ..d()
    });
    let rs = render_rust(&tree, root_id, None, false);
    assert!(rs.contains("*mut Target"));
}

#[test]
fn python_enum_slots() {
    let mut tree = NodeTree::new();
    tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Status".into(),
        name: "s".into(),
        class_keyword: "enum".into(),
        enum_members: vec![("OK".into(), 0), ("ERR".into(), 1)],
        ..d()
    });
    let py = render_python(&tree, tree.nodes[0].id);
    assert!(py.contains("__slots__"));
}

#[test]
fn cpp_hex128_in_union() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "U".into(),
        name: "u".into(),
        class_keyword: "union".into(),
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::Hex128,
        name: "big".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    let cpp = render_cpp(&tree, root_id, None, false);
    assert!(cpp.contains("union U"));
    assert!(cpp.contains("0x10") || cpp.contains("uint8_t"));
}

#[test]
fn rust_func_ptr_option() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "VT".into(),
        name: "vt".into(),
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::FuncPtr64,
        name: "fn".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    let rs = render_rust(&tree, root_id, None, false);
    assert!(rs.contains("Option<unsafe extern"));
}

#[test]
fn csharp_vec3() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "V".into(),
        name: "v".into(),
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::Vec3,
        name: "pos".into(),
        parent_id: root_id,
        offset: 0,
        ..d()
    });
    let cs = render_csharp(&tree, root_id, None, false);
    assert!(cs.contains("fixed float"));
    assert!(cs.contains("[3]"));
}

#[test]
fn defines_enum_members() {
    let mut tree = NodeTree::new();
    tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "Status".into(),
        name: "s".into(),
        class_keyword: "enum".into(),
        enum_members: vec![("OK".into(), 0), ("ERR".into(), 1)],
        ..d()
    });
    let def = render_defines(&tree, tree.nodes[0].id);
    assert!(def.contains("Status_OK 0"));
    assert!(def.contains("Status_ERR 1"));
}

#[test]
fn cpp_static_field_comment() {
    let mut tree = NodeTree::new();
    let ri = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "S".into(),
        name: "s".into(),
        ..d()
    });
    let root_id = tree.nodes[ri].id;
    tree.add_node(Node {
        kind: NodeKind::Hex64,
        name: "vtable".into(),
        parent_id: root_id,
        offset: 0,
        is_static: true,
        offset_expr: "base + 0x10".into(),
        ..d()
    });
    let cpp = render_cpp(&tree, root_id, None, false);
    assert!(cpp.contains("// static:"));
    assert!(cpp.contains("vtable"));
}

#[test]
fn cpp_type_aliases() {
    let tree = make_simple_struct();
    let mut aliases: TypeAliases = TypeAliases::new();
    aliases.insert(NodeKind::Int32, "LONG".into());
    let cpp = render_cpp(&tree, tree.nodes[0].id, Some(&aliases), false);
    assert!(cpp.contains("LONG"));
}

// ── Extra Rust-only determinism tests (spec §13) ──

#[test]
fn pad_counter_shared_across_render() {
    // Two roots each with a leading gap → expect sequential shared _pad numbering.
    let mut tree = NodeTree::new();
    let ai = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "A".into(),
        name: "a".into(),
        parent_id: 0,
        offset: 0,
        ..d()
    });
    let a_id = tree.nodes[ai].id;
    tree.add_node(Node {
        kind: NodeKind::UInt32,
        name: "x".into(),
        parent_id: a_id,
        offset: 4, // gap at 0..4
        ..d()
    });
    let bi = tree.add_node(Node {
        kind: NodeKind::Struct,
        struct_type_name: "B".into(),
        name: "b".into(),
        parent_id: 0,
        offset: 0x100,
        ..d()
    });
    let b_id = tree.nodes[bi].id;
    tree.add_node(Node {
        kind: NodeKind::UInt32,
        name: "y".into(),
        parent_id: b_id,
        offset: 4, // gap at 0..4
        ..d()
    });

    let result = render_cpp_all(&tree, None, false);
    assert!(result.contains("_pad0000"));
    assert!(result.contains("_pad0001"));
}
