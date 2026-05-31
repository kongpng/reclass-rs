//! Oracle-backed integration tests for the `imports` module.
//!
//! Translated 1:1 from the captured C++ QtTest sources:
//! - `_oracle/test_sources/test_import_source.cpp` (50 slots, 52 asserts)
//! - `_oracle/test_sources/test_import_xml.cpp` (importSmallXml)
//! - `_oracle/test_sources/test_export_xml.cpp` (10 slots)
//!
//! Run with: `cargo test --no-default-features --features imports`.

#![cfg(feature = "imports")]

use std::io::Write;

use reclass::core::kind::{size_for_kind, NodeKind};
use reclass::core::node::Node;
use reclass::core::tree::NodeTree;
use reclass::imports::{export_reclass_xml, import_from_source, import_reclass_xml};

// ── Helpers (mirror the C++ test helpers) ──

fn import8(src: &str) -> NodeTree {
    import_from_source(src, 8).expect("import_from_source should succeed")
}

fn count_roots(tree: &NodeTree) -> i32 {
    tree.nodes
        .iter()
        .filter(|n| n.parent_id == 0 && n.kind == NodeKind::Struct)
        .count() as i32
}

fn children_of(tree: &NodeTree, parent_id: u64) -> Vec<usize> {
    (0..tree.nodes.len())
        .filter(|&i| tree.nodes[i].parent_id == parent_id)
        .collect()
}

fn find_root(tree: &NodeTree, name: &str) -> usize {
    for (i, n) in tree.nodes.iter().enumerate() {
        if n.name == name && n.parent_id == 0 {
            return i;
        }
    }
    panic!("root {} not found", name);
}

// A temp file path unique to a test (avoids needing the tempfile crate).
fn temp_path(tag: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    p.push(format!("rcx_imports_{}_{}.reclass", tag, nanos));
    p
}

fn export_to_string(tree: &NodeTree) -> Option<String> {
    let path = temp_path("export");
    export_reclass_xml(tree, &path).ok()?;
    let s = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    Some(s)
}

fn round_trip(tree: &NodeTree) -> NodeTree {
    let path = temp_path("rt");
    export_reclass_xml(tree, &path).expect("export");
    let rt = import_reclass_xml(&path, 8).expect("reimport");
    let _ = std::fs::remove_file(&path);
    rt
}

// ════════════════════════════════════════════════════════════════════════
// test_import_source.cpp
// ════════════════════════════════════════════════════════════════════════

#[test]
fn empty_input() {
    let tree = import_from_source("", 8);
    assert!(tree.is_err());
}

#[test]
fn no_structs() {
    let tree = import_from_source("int x = 42;", 8);
    assert!(tree.is_err());
}

#[test]
fn single_empty_struct() {
    let tree = import8("struct Empty {};\n");
    assert_eq!(count_roots(&tree), 1);
    assert_eq!(tree.nodes[0].name, "Empty");
    assert_eq!(tree.nodes[0].kind, NodeKind::Struct);
}

#[test]
fn stdint_types() {
    let tree = import8(
        "struct Test {\n\
         uint8_t  a;\n\
         int8_t   b;\n\
         uint16_t c;\n\
         int16_t  d;\n\
         uint32_t e;\n\
         int32_t  f;\n\
         uint64_t g;\n\
         int64_t  h;\n\
         };\n",
    );
    assert_eq!(count_roots(&tree), 1);
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 8);
    let expect = [
        NodeKind::UInt8,
        NodeKind::Int8,
        NodeKind::UInt16,
        NodeKind::Int16,
        NodeKind::UInt32,
        NodeKind::Int32,
        NodeKind::UInt64,
        NodeKind::Int64,
    ];
    for (i, &k) in expect.iter().enumerate() {
        assert_eq!(tree.nodes[kids[i]].kind, k);
    }
}

#[test]
fn windows_types() {
    let tree = import8(
        "struct WinTypes {\n\
         BYTE a;\n WORD b;\n DWORD c;\n QWORD d;\n ULONG e;\n LONG f;\n\
         USHORT g;\n UCHAR h;\n BOOLEAN i;\n BOOL j;\n CHAR k;\n WCHAR l;\n\
         };\n",
    );
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 12);
    let expect = [
        NodeKind::UInt8,
        NodeKind::UInt16,
        NodeKind::UInt32,
        NodeKind::UInt64,
        NodeKind::UInt32,
        NodeKind::Int32,
        NodeKind::UInt16,
        NodeKind::UInt8,
        NodeKind::UInt8,
        NodeKind::Int32,
        NodeKind::Int8,
        NodeKind::UInt16,
    ];
    for (i, &k) in expect.iter().enumerate() {
        assert_eq!(tree.nodes[kids[i]].kind, k, "kid {}", i);
    }
}

#[test]
fn platform_pointer_types() {
    let tree = import8(
        "struct PtrTypes {\n\
         PVOID a;\n HANDLE b;\n SIZE_T c;\n ULONG_PTR d;\n uintptr_t e;\n size_t f;\n\
         };\n",
    );
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 6);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::Pointer64);
    assert_eq!(tree.nodes[kids[1]].kind, NodeKind::Pointer64);
    assert_eq!(tree.nodes[kids[2]].kind, NodeKind::UInt64);
    assert_eq!(tree.nodes[kids[3]].kind, NodeKind::UInt64);
    assert_eq!(tree.nodes[kids[4]].kind, NodeKind::UInt64);
    assert_eq!(tree.nodes[kids[5]].kind, NodeKind::UInt64);
}

#[test]
fn standard_c_types() {
    let tree = import8("struct CTypes {\n char a;\n short b;\n int c;\n long d;\n };\n");
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 4);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::Int8);
    assert_eq!(tree.nodes[kids[1]].kind, NodeKind::Int16);
    assert_eq!(tree.nodes[kids[2]].kind, NodeKind::Int32);
    assert_eq!(tree.nodes[kids[3]].kind, NodeKind::Int32);
}

#[test]
fn multi_word_types() {
    let tree = import8(
        "struct MultiWord {\n\
         unsigned char a;\n unsigned short b;\n unsigned int c;\n\
         unsigned long d;\n long long e;\n unsigned long long f;\n};\n",
    );
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 6);
    let expect = [
        NodeKind::UInt8,
        NodeKind::UInt16,
        NodeKind::UInt32,
        NodeKind::UInt32,
        NodeKind::Int64,
        NodeKind::UInt64,
    ];
    for (i, &k) in expect.iter().enumerate() {
        assert_eq!(tree.nodes[kids[i]].kind, k);
    }
}

#[test]
fn float_double() {
    let tree = import8("struct FD {\n float a;\n double b;\n };\n");
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 2);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::Float);
    assert_eq!(tree.nodes[kids[1]].kind, NodeKind::Double);
}

#[test]
fn bool_type() {
    let tree = import8("struct B {\n bool a;\n _Bool b;\n };\n");
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 2);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::Bool);
    assert_eq!(tree.nodes[kids[1]].kind, NodeKind::Bool);
}

#[test]
fn void_pointer() {
    let tree = import8("struct VP {\n void* ptr;\n };\n");
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 1);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::Pointer64);
    assert_eq!(tree.nodes[kids[0]].name, "ptr");
    assert_eq!(tree.nodes[kids[0]].ref_id, 0); // void* has no target
}

#[test]
fn typed_pointer() {
    let tree = import8(
        "struct Target {\n int x;\n };\n\
         struct HasPtr {\n Target* pTarget;\n };\n",
    );
    assert_eq!(count_roots(&tree), 2);
    let has_ptr = find_root(&tree, "HasPtr");
    let kids = children_of(&tree, tree.nodes[has_ptr].id);
    assert_eq!(kids.len(), 1);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::Pointer64);
    assert_ne!(tree.nodes[kids[0]].ref_id, 0);
    let target_idx = tree.index_of_id(tree.nodes[kids[0]].ref_id);
    assert!(target_idx >= 0);
    assert_eq!(tree.nodes[target_idx as usize].name, "Target");
}

#[test]
fn self_referencing_pointer() {
    let tree = import8("struct Node {\n int value;\n Node* next;\n };\n");
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 2);
    assert_eq!(tree.nodes[kids[1]].kind, NodeKind::Pointer64);
    assert_eq!(tree.nodes[kids[1]].ref_id, tree.nodes[0].id);
}

#[test]
fn double_pointer() {
    let tree = import8("struct DP {\n void** ppData;\n };\n");
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 1);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::Pointer64);
}

#[test]
fn primitive_array() {
    let tree = import8("struct PA {\n int32_t values[10];\n };\n");
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 1);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::Array);
    assert_eq!(tree.nodes[kids[0]].array_len, 10);
    assert_eq!(tree.nodes[kids[0]].element_kind, NodeKind::Int32);
}

#[test]
fn char_array_to_utf8() {
    let tree = import8("struct CA {\n char name[64];\n };\n");
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 1);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::UTF8);
    assert_eq!(tree.nodes[kids[0]].str_len, 64);
}

#[test]
fn wchar_array_to_utf16() {
    let tree = import8("struct WC {\n wchar_t name[32];\n };\n");
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 1);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::UTF16);
    assert_eq!(tree.nodes[kids[0]].str_len, 32);
}

#[test]
fn float_array_to_vec2() {
    let tree = import8("struct V {\n float pos[2];\n };\n");
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 1);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::Vec2);
}

#[test]
fn float_array_to_vec3() {
    let tree = import8("struct V {\n float pos[3];\n };\n");
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::Vec3);
}

#[test]
fn float_array_to_vec4() {
    let tree = import8("struct V {\n float rot[4];\n };\n");
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::Vec4);
}

#[test]
fn float_array_4x4_to_mat4x4() {
    let tree = import8("struct M {\n float matrix[4][4];\n };\n");
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::Mat4x4);
}

#[test]
fn generic_float_array() {
    let tree = import8("struct GF {\n float values[8];\n };\n");
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 1);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::Array);
    assert_eq!(tree.nodes[kids[0]].array_len, 8);
    assert_eq!(tree.nodes[kids[0]].element_kind, NodeKind::Float);
}

#[test]
fn struct_array() {
    let tree = import8(
        "struct Item {\n int id;\n };\n\
         struct Container {\n Item items[5];\n };\n",
    );
    assert_eq!(count_roots(&tree), 2);
    let cont = find_root(&tree, "Container");
    let kids = children_of(&tree, tree.nodes[cont].id);
    assert_eq!(kids.len(), 1);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::Array);
    assert_eq!(tree.nodes[kids[0]].array_len, 5);
    assert_eq!(tree.nodes[kids[0]].element_kind, NodeKind::Struct);
}

#[test]
fn comment_offsets() {
    let tree = import8(
        "struct Offsets {\n\
         uint64_t vtable; // 0x0\n\
         float health; // 0x8\n\
         uint8_t _pad000C[0x4]; // 0xC\n\
         double score; // 0x10\n\
         };\n",
    );
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(tree.nodes[kids[0]].offset, 0);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::UInt64);
    assert_eq!(tree.nodes[kids[1]].offset, 8);
    assert_eq!(tree.nodes[kids[1]].kind, NodeKind::Float);
    let mut found_double = false;
    for &k in &kids {
        if tree.nodes[k].kind == NodeKind::Double {
            assert_eq!(tree.nodes[k].offset, 0x10);
            found_double = true;
        }
    }
    assert!(found_double);
}

#[test]
fn computed_offsets() {
    let tree =
        import8("struct Computed {\n uint8_t a;\n uint16_t b;\n uint32_t c;\n uint64_t d;\n };\n");
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 4);
    assert_eq!(tree.nodes[kids[0]].offset, 0);
    assert_eq!(tree.nodes[kids[1]].offset, 2);
    assert_eq!(tree.nodes[kids[2]].offset, 4);
    assert_eq!(tree.nodes[kids[3]].offset, 8);
}

#[test]
fn mixed_offsets_auto_detect() {
    let tree = import8(
        "struct Mixed {\n\
         uint32_t a; // 0x0\n\
         uint32_t b;\n\
         uint32_t c; // 0x10\n\
         };\n",
    );
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(tree.nodes[kids[0]].offset, 0);
    assert_eq!(tree.nodes[kids[1]].offset, 4);
    assert_eq!(tree.nodes[kids[2]].offset, 0x10);
}

#[test]
fn multi_struct() {
    let tree = import8(
        "struct A {\n int x;\n };\n\
         struct B {\n float y;\n };\n\
         struct C {\n double z;\n };\n",
    );
    assert_eq!(count_roots(&tree), 3);
}

#[test]
fn pointer_cross_ref() {
    let tree = import8(
        "struct A {\n int value;\n };\n\
         struct B {\n A* ref;\n };\n",
    );
    let b = find_root(&tree, "B");
    let kids = children_of(&tree, tree.nodes[b].id);
    assert_eq!(kids.len(), 1);
    assert_ne!(tree.nodes[kids[0]].ref_id, 0);
    let a_idx = tree.index_of_id(tree.nodes[kids[0]].ref_id);
    assert!(a_idx >= 0);
    assert_eq!(tree.nodes[a_idx as usize].name, "A");
}

#[test]
fn forward_declaration() {
    let tree = import8(
        "struct Bar;\n\
         struct Foo {\n Bar* pBar;\n };\n\
         struct Bar {\n int val;\n };\n",
    );
    assert_eq!(count_roots(&tree), 2);
    let foo = find_root(&tree, "Foo");
    let kids = children_of(&tree, tree.nodes[foo].id);
    assert_eq!(kids.len(), 1);
    assert_ne!(tree.nodes[kids[0]].ref_id, 0);
}

#[test]
fn union_container() {
    let tree = import8(
        "struct WithUnion {\n\
         union {\n float asFloat;\n uint32_t asInt;\n };\n\
         int after;\n\
         };\n",
    );
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 2);

    let union_node = &tree.nodes[kids[0]];
    assert_eq!(union_node.kind, NodeKind::Struct);
    assert_eq!(union_node.class_keyword, "union");
    assert_eq!(union_node.offset, 0);
    let union_id = union_node.id;

    let union_kids = children_of(&tree, union_id);
    assert_eq!(union_kids.len(), 2);
    assert_eq!(tree.nodes[union_kids[0]].kind, NodeKind::Float);
    assert_eq!(tree.nodes[union_kids[0]].name, "asFloat");
    assert_eq!(tree.nodes[union_kids[0]].offset, 0);
    assert_eq!(tree.nodes[union_kids[1]].kind, NodeKind::UInt32);
    assert_eq!(tree.nodes[union_kids[1]].name, "asInt");
    assert_eq!(tree.nodes[union_kids[1]].offset, 0);

    assert_eq!(tree.struct_span(union_id), 4);

    assert_eq!(tree.nodes[kids[1]].kind, NodeKind::Int32);
    assert_eq!(tree.nodes[kids[1]].name, "after");
    assert_eq!(tree.nodes[kids[1]].offset, 4);
}

#[test]
fn union_with_comment_offsets() {
    let tree = import8(
        "struct S {\n\
         uint64_t a; // 0x0\n\
         union {\n uint32_t x; // 0x8\n float y; // 0x8\n };\n\
         uint32_t b; // 0xC\n\
         };\n",
    );
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 3);

    let union_node = &tree.nodes[kids[1]];
    assert_eq!(union_node.kind, NodeKind::Struct);
    assert_eq!(union_node.class_keyword, "union");
    assert_eq!(union_node.offset, 0x8);
    let union_id = union_node.id;

    let union_kids = children_of(&tree, union_id);
    assert_eq!(union_kids.len(), 2);
    assert_eq!(tree.nodes[union_kids[0]].offset, 0);
    assert_eq!(tree.nodes[union_kids[1]].offset, 0);

    assert_eq!(tree.nodes[kids[2]].offset, 0xC);
}

#[test]
fn named_union() {
    let tree = import8(
        "struct S {\n\
         union {\n uint16_t shortVal;\n uint64_t longVal;\n } u3;\n\
         };\n",
    );
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 1);

    let union_node = &tree.nodes[kids[0]];
    assert_eq!(union_node.kind, NodeKind::Struct);
    assert_eq!(union_node.class_keyword, "union");
    assert_eq!(union_node.name, "u3");
    let union_id = union_node.id;

    let union_kids = children_of(&tree, union_id);
    assert_eq!(union_kids.len(), 2);
    assert_eq!(tree.struct_span(union_id), 8);
}

#[test]
fn padding_field_expansion() {
    let tree = import8("struct Padded {\n uint8_t _pad0000[0x10];\n };\n");
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 2);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::Hex64);
    assert_eq!(tree.nodes[kids[0]].offset, 0);
    assert_eq!(tree.nodes[kids[1]].kind, NodeKind::Hex64);
    assert_eq!(tree.nodes[kids[1]].offset, 8);
}

#[test]
fn static_assert_tail_padding() {
    let tree = import8(
        "struct Sized {\n uint32_t x;\n };\n\
         static_assert(sizeof(Sized) == 0x10, \"Size check\");\n",
    );
    let span = tree.struct_span(tree.nodes[0].id);
    assert_eq!(span, 0x10);
}

#[test]
fn embedded_struct() {
    let tree = import8(
        "struct Inner {\n int a;\n };\n\
         struct Outer {\n Inner embedded;\n float after;\n };\n",
    );
    let outer = find_root(&tree, "Outer");
    let kids = children_of(&tree, tree.nodes[outer].id);
    assert_eq!(kids.len(), 2);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::Struct);
    assert_eq!(tree.nodes[kids[0]].struct_type_name, "Inner");
    assert_ne!(tree.nodes[kids[0]].ref_id, 0);
    assert_eq!(tree.nodes[kids[1]].kind, NodeKind::Float);
}

#[test]
fn typedef_basic() {
    let tree = import8(
        "typedef uint32_t MyInt;\n\
         struct TD {\n MyInt value;\n };\n",
    );
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 1);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::UInt32);
}

#[test]
fn const_volatile_qualifiers() {
    let tree = import8(
        "struct Quals {\n\
         const uint32_t a;\n volatile int32_t b;\n const volatile uint8_t c;\n\
         };\n",
    );
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 3);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::UInt32);
    assert_eq!(tree.nodes[kids[1]].kind, NodeKind::Int32);
    assert_eq!(tree.nodes[kids[2]].kind, NodeKind::UInt8);
}

#[test]
fn struct_prefix_on_type() {
    let tree = import8(
        "struct Inner {\n int val;\n };\n\
         struct Outer {\n struct Inner member;\n };\n",
    );
    let outer = find_root(&tree, "Outer");
    let kids = children_of(&tree, tree.nodes[outer].id);
    assert_eq!(kids.len(), 1);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::Struct);
    assert_eq!(tree.nodes[kids[0]].struct_type_name, "Inner");
}

#[test]
fn bitfield_skipped() {
    let tree = import8(
        "struct BF {\n\
         uint32_t normal;\n\
         uint32_t bitA : 4;\n\
         uint32_t bitB : 12;\n\
         uint32_t after;\n\
         };\n",
    );
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 3);
    assert_eq!(tree.nodes[kids[0]].name, "normal");
    assert_eq!(tree.nodes[kids[0]].offset, 0);
    assert_eq!(tree.nodes[kids[1]].kind, NodeKind::Struct);
    assert_eq!(tree.nodes[kids[1]].resolved_class_keyword(), "bitfield");
    assert_eq!(tree.nodes[kids[1]].offset, 4);
    assert_eq!(tree.nodes[kids[1]].bitfield_members.len(), 2);
    assert_eq!(tree.nodes[kids[1]].bitfield_members[0].name, "bitA");
    assert_eq!(tree.nodes[kids[1]].bitfield_members[0].bit_width, 4);
    assert_eq!(tree.nodes[kids[1]].bitfield_members[0].bit_offset, 0);
    assert_eq!(tree.nodes[kids[1]].bitfield_members[1].name, "bitB");
    assert_eq!(tree.nodes[kids[1]].bitfield_members[1].bit_width, 12);
    assert_eq!(tree.nodes[kids[1]].bitfield_members[1].bit_offset, 4);
    assert_eq!(tree.nodes[kids[2]].name, "after");
    assert_eq!(tree.nodes[kids[2]].offset, 8);
}

#[test]
fn bitfield_with_offsets_emits_hex() {
    let tree = import8(
        "struct BF2 {\n\
         uint32_t normal; // 0x0\n\
         ULONGLONG Valid : 1; // 0x4\n\
         ULONGLONG Dirty : 1; // 0x4\n\
         ULONGLONG PageFrameNumber : 36; // 0x4\n\
         ULONGLONG Reserved : 26; // 0x4\n\
         uint32_t after; // 0xC\n\
         };\n",
    );
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 3);
    assert_eq!(tree.nodes[kids[0]].name, "normal");
    assert_eq!(tree.nodes[kids[0]].offset, 0);
    assert_eq!(tree.nodes[kids[1]].kind, NodeKind::Struct);
    assert_eq!(tree.nodes[kids[1]].resolved_class_keyword(), "bitfield");
    assert_eq!(tree.nodes[kids[1]].offset, 4);
    assert_eq!(tree.nodes[kids[1]].element_kind, NodeKind::Hex64);
    assert_eq!(tree.nodes[kids[1]].bitfield_members.len(), 4);
    assert_eq!(tree.nodes[kids[1]].bitfield_members[0].name, "Valid");
    assert_eq!(tree.nodes[kids[1]].bitfield_members[0].bit_width, 1);
    assert_eq!(tree.nodes[kids[1]].bitfield_members[1].name, "Dirty");
    assert_eq!(
        tree.nodes[kids[1]].bitfield_members[2].name,
        "PageFrameNumber"
    );
    assert_eq!(tree.nodes[kids[1]].bitfield_members[2].bit_width, 36);
    assert_eq!(tree.nodes[kids[1]].bitfield_members[3].name, "Reserved");
    assert_eq!(tree.nodes[kids[2]].name, "after");
    assert_eq!(tree.nodes[kids[2]].offset, 0xC);
}

#[test]
fn hex_array_sizes() {
    let tree = import8("struct HexArr {\n uint8_t data[0x20];\n };\n");
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 1);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::Array);
    assert_eq!(tree.nodes[kids[0]].array_len, 0x20);
}

#[test]
fn windows_style_peb() {
    let tree = import8(
        "struct PEB64 {\n\
         BOOLEAN InheritedAddressSpace;\n\
         BOOLEAN ReadImageFileExecOptions;\n\
         BOOLEAN BeingDebugged;\n\
         BOOLEAN BitField;\n\
         PVOID Mutant;\n\
         PVOID ImageBaseAddress;\n\
         };\n",
    );
    assert_eq!(count_roots(&tree), 1);
    assert_eq!(tree.nodes[0].name, "PEB64");
    let kids = children_of(&tree, tree.nodes[0].id);
    assert_eq!(kids.len(), 6);
    for i in 0..4 {
        assert_eq!(tree.nodes[kids[i]].kind, NodeKind::UInt8);
    }
    assert_eq!(tree.nodes[kids[4]].kind, NodeKind::Pointer64);
    assert_eq!(tree.nodes[kids[5]].kind, NodeKind::Pointer64);
}

#[test]
fn class_keyword() {
    let tree = import8("class MyClass {\n int value;\n };\n");
    assert_eq!(count_roots(&tree), 1);
    assert_eq!(tree.nodes[0].class_keyword, "class");
}

#[test]
fn inheritance_skipped() {
    let tree = import8(
        "struct Base {\n int a;\n };\n\
         struct Derived : public Base {\n float b;\n };\n",
    );
    assert_eq!(count_roots(&tree), 2);
    let derived = find_root(&tree, "Derived");
    let kids = children_of(&tree, tree.nodes[derived].id);
    assert_eq!(kids.len(), 1);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::Float);
}

#[test]
fn enum_basic() {
    let tree = import8("enum Color { Red = 0, Green = 1, Blue = 2 };");
    assert_eq!(count_roots(&tree), 1);
    assert_eq!(tree.nodes[0].class_keyword, "enum");
    assert_eq!(tree.nodes[0].struct_type_name, "Color");
    assert_eq!(tree.nodes[0].enum_members.len(), 3);
    assert_eq!(tree.nodes[0].enum_members[0].0, "Red");
    assert_eq!(tree.nodes[0].enum_members[0].1, 0i64);
    assert_eq!(tree.nodes[0].enum_members[1].0, "Green");
    assert_eq!(tree.nodes[0].enum_members[1].1, 1i64);
    assert_eq!(tree.nodes[0].enum_members[2].0, "Blue");
    assert_eq!(tree.nodes[0].enum_members[2].1, 2i64);
}

#[test]
fn enum_auto_values() {
    let tree = import8("enum Flags { A, B, C };");
    assert_eq!(tree.nodes[0].enum_members.len(), 3);
    assert_eq!(tree.nodes[0].enum_members[0].1, 0i64);
    assert_eq!(tree.nodes[0].enum_members[1].1, 1i64);
    assert_eq!(tree.nodes[0].enum_members[2].1, 2i64);
}

#[test]
fn enum_hex_values() {
    let tree = import8("enum Hex { X = 0x10, Y = 0x20 };");
    assert_eq!(tree.nodes[0].enum_members.len(), 2);
    assert_eq!(tree.nodes[0].enum_members[0].1, 0x10i64);
    assert_eq!(tree.nodes[0].enum_members[1].1, 0x20i64);
}

#[test]
fn enum_in_struct() {
    let tree = import8(
        "enum PoolType { NonPaged = 0, Paged = 1 };\n\
         struct Foo {\n PoolType pool; //0x0\n uint32_t size; //0x4\n };",
    );
    assert_eq!(count_roots(&tree), 2);
    let foo = find_root(&tree, "Foo");
    let kids = children_of(&tree, tree.nodes[foo].id);
    assert_eq!(kids.len(), 2);
    assert_eq!(tree.nodes[kids[0]].kind, NodeKind::UInt32);
    assert_eq!(tree.nodes[kids[0]].name, "pool");
    assert_ne!(tree.nodes[kids[0]].ref_id, 0);
}

#[test]
fn enum_class() {
    let tree = import8("enum class Scope : uint8_t { A = 1, B = 2 };");
    assert_eq!(count_roots(&tree), 1);
    assert_eq!(tree.nodes[0].class_keyword, "enum");
    assert_eq!(tree.nodes[0].struct_type_name, "Scope");
    assert_eq!(tree.nodes[0].enum_members.len(), 2);
    assert_eq!(tree.nodes[0].enum_members[0].0, "A");
    assert_eq!(tree.nodes[0].enum_members[0].1, 1i64);
}

#[test]
fn basic_round_trip() {
    // The original builds a tree manually then re-imports matching source text.
    let mut original = NodeTree::default();
    let s_idx = original.add_node(Node {
        kind: NodeKind::Struct,
        name: "RoundTrip".into(),
        struct_type_name: "RoundTrip".into(),
        parent_id: 0,
        offset: 0,
        ..Node::default()
    });
    let s_id = original.nodes[s_idx].id;
    original.add_node(Node {
        kind: NodeKind::UInt32,
        name: "field_a".into(),
        parent_id: s_id,
        offset: 0,
        ..Node::default()
    });
    original.add_node(Node {
        kind: NodeKind::Float,
        name: "field_b".into(),
        parent_id: s_id,
        offset: 4,
        ..Node::default()
    });
    original.add_node(Node {
        kind: NodeKind::UInt64,
        name: "field_c".into(),
        parent_id: s_id,
        offset: 8,
        ..Node::default()
    });

    let source = "struct RoundTrip {\n\
         uint32_t field_a; // 0x0\n\
         float field_b; // 0x4\n\
         uint64_t field_c; // 0x8\n\
         };\n\
         static_assert(sizeof(RoundTrip) == 0x10, \"Size mismatch\");\n";

    let reimported = import8(source);
    assert_eq!(count_roots(&reimported), 1);
    assert_eq!(reimported.nodes[0].name, "RoundTrip");

    let orig_kids = children_of(&original, original.nodes[0].id);
    let reimp_kids = children_of(&reimported, reimported.nodes[0].id);
    assert!(reimp_kids.len() >= 3);
    for i in 0..3 {
        assert_eq!(
            reimported.nodes[reimp_kids[i]].kind,
            original.nodes[orig_kids[i]].kind
        );
        assert_eq!(
            reimported.nodes[reimp_kids[i]].name,
            original.nodes[orig_kids[i]].name
        );
        assert_eq!(
            reimported.nodes[reimp_kids[i]].offset,
            original.nodes[orig_kids[i]].offset
        );
    }
}

// ════════════════════════════════════════════════════════════════════════
// test_import_xml.cpp
// ════════════════════════════════════════════════════════════════════════

#[test]
fn import_small_xml() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ReClass>
    <!--ReClassEx-->
    <Class Name="TestClass" Type="28" Comment="" Offset="0" strOffset="0" Code="">
        <Node Name="vtable" Type="9" Size="8" bHidden="false" Comment=""/>
        <Node Name="health" Type="13" Size="4" bHidden="false" Comment=""/>
        <Node Name="name" Type="18" Size="32" bHidden="false" Comment=""/>
        <Node Name="position" Type="23" Size="12" bHidden="false" Comment=""/>
        <Node Name="pNext" Type="8" Size="8" bHidden="false" Comment="" Pointer="TestClass"/>
    </Class>
</ReClass>
"#;
    let path = temp_path("small_xml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(xml.as_bytes()).unwrap();
    }
    let tree = import_reclass_xml(&path, 8).expect("import xml");
    let _ = std::fs::remove_file(&path);

    assert_eq!(tree.nodes.len(), 6);
    assert_eq!(tree.nodes[0].kind, NodeKind::Struct);
    assert_eq!(tree.nodes[0].name, "TestClass");

    assert_eq!(tree.nodes[1].kind, NodeKind::Int64);
    assert_eq!(tree.nodes[1].name, "vtable");
    assert_eq!(tree.nodes[1].offset, 0);

    assert_eq!(tree.nodes[2].kind, NodeKind::Float);
    assert_eq!(tree.nodes[2].name, "health");
    assert_eq!(tree.nodes[2].offset, 8);

    assert_eq!(tree.nodes[3].kind, NodeKind::UTF8);
    assert_eq!(tree.nodes[3].str_len, 32);
    assert_eq!(tree.nodes[3].offset, 12);

    assert_eq!(tree.nodes[4].kind, NodeKind::Vec3);
    assert_eq!(tree.nodes[4].offset, 44);

    assert_eq!(tree.nodes[5].kind, NodeKind::Pointer64);
    assert_eq!(tree.nodes[5].name, "pNext");
    assert_ne!(tree.nodes[5].ref_id, 0);
    assert_eq!(tree.nodes[5].ref_id, tree.nodes[0].id);
}

// ════════════════════════════════════════════════════════════════════════
// test_export_xml.cpp
// ════════════════════════════════════════════════════════════════════════

fn mk_struct(tree: &mut NodeTree, name: &str) -> u64 {
    let idx = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: name.into(),
        struct_type_name: name.into(),
        parent_id: 0,
        ..Node::default()
    });
    tree.nodes[idx].id
}

#[test]
fn export_empty_tree() {
    let tree = NodeTree::default();
    let r = export_reclass_xml(&tree, std::path::Path::new("dummy.xml"));
    assert!(r.is_err());
}

#[test]
fn export_single_struct() {
    let mut tree = NodeTree::default();
    let sid = mk_struct(&mut tree, "Player");
    tree.add_node(Node {
        kind: NodeKind::Int32,
        name: "health".into(),
        parent_id: sid,
        offset: 0,
        ..Node::default()
    });
    tree.add_node(Node {
        kind: NodeKind::Float,
        name: "speed".into(),
        parent_id: sid,
        offset: 4,
        ..Node::default()
    });
    tree.add_node(Node {
        kind: NodeKind::UInt64,
        name: "id".into(),
        parent_id: sid,
        offset: 8,
        ..Node::default()
    });

    let xml = export_to_string(&tree).expect("export");
    assert!(xml.contains("Player"));
    assert!(xml.contains("health"));
    assert!(xml.contains("speed"));
    assert!(xml.contains("ReClassEx"));

    let rt = round_trip(&tree);
    assert_eq!(count_roots(&rt), 1);
    assert_eq!(rt.nodes[0].name, "Player");
    let kids = children_of(&rt, rt.nodes[0].id);
    assert_eq!(kids.len(), 3);
    assert_eq!(rt.nodes[kids[0]].kind, NodeKind::Int32);
    assert_eq!(rt.nodes[kids[1]].kind, NodeKind::Float);
    assert_eq!(rt.nodes[kids[2]].kind, NodeKind::UInt64);
}

#[test]
fn export_pointer_ref() {
    let mut tree = NodeTree::default();
    let s1id = mk_struct(&mut tree, "Target");
    tree.add_node(Node {
        kind: NodeKind::Int32,
        name: "val".into(),
        parent_id: s1id,
        offset: 0,
        ..Node::default()
    });
    let s2id = mk_struct(&mut tree, "HasPtr");
    tree.add_node(Node {
        kind: NodeKind::Pointer64,
        name: "pTarget".into(),
        parent_id: s2id,
        offset: 0,
        ref_id: s1id,
        ..Node::default()
    });

    let xml = export_to_string(&tree).expect("export");
    assert!(xml.contains("Pointer=\"Target\""));

    let rt = round_trip(&tree);
    assert_eq!(count_roots(&rt), 2);
    let mut found = false;
    for n in &rt.nodes {
        if n.kind == NodeKind::Pointer64 && n.name == "pTarget" {
            assert_ne!(n.ref_id, 0);
            found = true;
        }
    }
    assert!(found);
}

#[test]
fn export_embedded_struct() {
    let mut tree = NodeTree::default();
    let iid = mk_struct(&mut tree, "Inner");
    tree.add_node(Node {
        kind: NodeKind::Int32,
        name: "x".into(),
        parent_id: iid,
        offset: 0,
        ..Node::default()
    });
    let oid = mk_struct(&mut tree, "Outer");
    tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "embedded".into(),
        struct_type_name: "Inner".into(),
        parent_id: oid,
        offset: 0,
        ref_id: iid,
        ..Node::default()
    });

    let xml = export_to_string(&tree).expect("export");
    assert!(xml.contains("Instance=\"Inner\""));
}

#[test]
fn export_array() {
    let mut tree = NodeTree::default();
    let sid = mk_struct(&mut tree, "Container");
    tree.add_node(Node {
        kind: NodeKind::Array,
        name: "items".into(),
        parent_id: sid,
        offset: 0,
        array_len: 10,
        element_kind: NodeKind::Int32,
        ..Node::default()
    });

    let xml = export_to_string(&tree).expect("export");
    assert!(xml.contains("Total=\"10\""));
    assert!(xml.contains("<Array"));
}

#[test]
fn export_text_nodes() {
    let mut tree = NodeTree::default();
    let sid = mk_struct(&mut tree, "TextStruct");
    tree.add_node(Node {
        kind: NodeKind::UTF8,
        name: "name".into(),
        parent_id: sid,
        offset: 0,
        str_len: 32,
        ..Node::default()
    });
    tree.add_node(Node {
        kind: NodeKind::UTF16,
        name: "wname".into(),
        parent_id: sid,
        offset: 32,
        str_len: 16,
        ..Node::default()
    });

    let rt = round_trip(&tree);
    assert_eq!(count_roots(&rt), 1);
    let kids = children_of(&rt, rt.nodes[0].id);
    assert_eq!(kids.len(), 2);
    assert_eq!(rt.nodes[kids[0]].kind, NodeKind::UTF8);
    assert_eq!(rt.nodes[kids[0]].str_len, 32);
    assert_eq!(rt.nodes[kids[1]].kind, NodeKind::UTF16);
    assert_eq!(rt.nodes[kids[1]].str_len, 16);
}

#[test]
fn export_vectors() {
    let mut tree = NodeTree::default();
    let sid = mk_struct(&mut tree, "Vectors");
    tree.add_node(Node {
        kind: NodeKind::Vec2,
        name: "pos2".into(),
        parent_id: sid,
        offset: 0,
        ..Node::default()
    });
    tree.add_node(Node {
        kind: NodeKind::Vec3,
        name: "pos3".into(),
        parent_id: sid,
        offset: 8,
        ..Node::default()
    });
    tree.add_node(Node {
        kind: NodeKind::Vec4,
        name: "rot".into(),
        parent_id: sid,
        offset: 20,
        ..Node::default()
    });
    tree.add_node(Node {
        kind: NodeKind::Mat4x4,
        name: "matrix".into(),
        parent_id: sid,
        offset: 36,
        ..Node::default()
    });

    let rt = round_trip(&tree);
    let kids = children_of(&rt, rt.nodes[0].id);
    assert_eq!(kids.len(), 4);
    assert_eq!(rt.nodes[kids[0]].kind, NodeKind::Vec2);
    assert_eq!(rt.nodes[kids[1]].kind, NodeKind::Vec3);
    assert_eq!(rt.nodes[kids[2]].kind, NodeKind::Vec4);
    assert_eq!(rt.nodes[kids[3]].kind, NodeKind::Mat4x4);
}

#[test]
fn export_hex_collapse() {
    let mut tree = NodeTree::default();
    let sid = mk_struct(&mut tree, "HexTest");
    for i in 0..4 {
        tree.add_node(Node {
            kind: NodeKind::Hex8,
            parent_id: sid,
            offset: i,
            ..Node::default()
        });
    }
    tree.add_node(Node {
        kind: NodeKind::Int32,
        name: "val".into(),
        parent_id: sid,
        offset: 4,
        ..Node::default()
    });

    let xml = export_to_string(&tree).expect("export");
    assert!(xml.contains("Type=\"21\""));
    assert!(xml.contains("Size=\"4\""));

    let rt = round_trip(&tree);
    assert_eq!(count_roots(&rt), 1);
    let kids = children_of(&rt, rt.nodes[0].id);
    assert!(kids.len() >= 2);
    let last = *kids.last().unwrap();
    assert_eq!(rt.nodes[last].kind, NodeKind::Int32);
}

#[test]
fn export_multi_class() {
    let mut tree = NodeTree::default();
    for c in 0..5 {
        let name = format!("Class{}", c);
        let sid = mk_struct(&mut tree, &name);
        tree.add_node(Node {
            kind: NodeKind::Int32,
            name: format!("field{}", c),
            parent_id: sid,
            offset: 0,
            ..Node::default()
        });
    }

    let rt = round_trip(&tree);
    assert_eq!(count_roots(&rt), 5);
    let names: std::collections::HashSet<String> = rt
        .nodes
        .iter()
        .filter(|n| n.parent_id == 0 && n.kind == NodeKind::Struct)
        .map(|n| n.name.clone())
        .collect();
    for c in 0..5 {
        assert!(names.contains(&format!("Class{}", c)));
    }
}

#[test]
fn round_trip_import_export() {
    let mut tree = NodeTree::default();
    let sid = mk_struct(&mut tree, "FullTest");

    let mut offset = 0i32;
    let mut add_field = |tree: &mut NodeTree, kind: NodeKind, name: &str| {
        tree.add_node(Node {
            kind,
            name: name.into(),
            parent_id: sid,
            offset,
            ..Node::default()
        });
        offset += size_for_kind(kind);
    };

    add_field(&mut tree, NodeKind::Int8, "a");
    add_field(&mut tree, NodeKind::Int16, "b");
    add_field(&mut tree, NodeKind::Int32, "c");
    add_field(&mut tree, NodeKind::Int64, "d");
    add_field(&mut tree, NodeKind::UInt8, "e");
    add_field(&mut tree, NodeKind::UInt16, "f");
    add_field(&mut tree, NodeKind::UInt32, "g");
    add_field(&mut tree, NodeKind::UInt64, "h");
    add_field(&mut tree, NodeKind::Float, "i");
    add_field(&mut tree, NodeKind::Double, "j");
    add_field(&mut tree, NodeKind::Vec2, "k");
    add_field(&mut tree, NodeKind::Vec3, "l");
    add_field(&mut tree, NodeKind::Vec4, "m");

    // Self-pointer
    tree.add_node(Node {
        kind: NodeKind::Pointer64,
        name: "self".into(),
        parent_id: sid,
        offset,
        ref_id: sid,
        ..Node::default()
    });
    offset += 8;

    // UTF8
    tree.add_node(Node {
        kind: NodeKind::UTF8,
        name: "str".into(),
        parent_id: sid,
        offset,
        str_len: 64,
        ..Node::default()
    });

    let rt = round_trip(&tree);
    assert_eq!(count_roots(&rt), 1);
    assert_eq!(rt.nodes[0].name, "FullTest");

    let orig_kids = children_of(&tree, sid);
    let rt_kids = children_of(&rt, rt.nodes[0].id);
    assert_eq!(rt_kids.len(), orig_kids.len());

    for i in 0..orig_kids.len() {
        assert_eq!(
            rt.nodes[rt_kids[i]].kind, tree.nodes[orig_kids[i]].kind,
            "field {} kind",
            i
        );
        assert_eq!(
            rt.nodes[rt_kids[i]].name, tree.nodes[orig_kids[i]].name,
            "field {} name",
            i
        );
    }

    let mut found_self = false;
    for n in &rt.nodes {
        if n.name == "self" && n.kind == NodeKind::Pointer64 {
            assert_ne!(n.ref_id, 0);
            assert_eq!(n.ref_id, rt.nodes[0].id);
            found_self = true;
        }
    }
    assert!(found_self);
}
