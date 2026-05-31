//! Predefined struct templates (`CommonType` / `CommonField`) and
//! `find_common_type()`.
//!
//! Faithful port of `src/commontypes.h` (405 lines). The C++ stores these as
//! static `constexpr` tables of `const char*` + `NodeKind`; the Rust port keeps
//! them as a `const [CommonType; N]` of `&'static str` fields. Pure data — when
//! the user picks one from the type chooser, a struct is created with these
//! exact fields instead of blank hex padding.

use super::kind::NodeKind;

/// `struct CommonField` (`commontypes.h:11-17`).
#[derive(Copy, Clone, Debug)]
pub struct CommonField {
    pub offset: i32,
    pub kind: NodeKind,
    pub name: &'static str,
    /// For pointer fields: target type name (empty = `void*`).
    pub ptr_target: &'static str,
}

/// `struct CommonType` (`commontypes.h:19-26`).
#[derive(Copy, Clone, Debug)]
pub struct CommonType {
    /// e.g. "_M128A".
    pub name: &'static str,
    /// e.g. "Windows NT", "C++ STL".
    pub category: &'static str,
    /// "struct" / "union" / "class".
    pub class_keyword: &'static str,
    /// total size in bytes.
    pub total_size: i32,
    pub fields: &'static [CommonField],
}

// Field-row constructors keep the table below readable (mirrors the brace-init
// rows of the C++ `kFields_*` arrays).
const fn f(offset: i32, kind: NodeKind, name: &'static str) -> CommonField {
    CommonField {
        offset,
        kind,
        name,
        ptr_target: "",
    }
}
const fn fp(
    offset: i32,
    kind: NodeKind,
    name: &'static str,
    ptr_target: &'static str,
) -> CommonField {
    CommonField {
        offset,
        kind,
        name,
        ptr_target,
    }
}

use NodeKind::*;

// ── Windows NT ──
const M128A: &[CommonField] = &[f(0x00, UInt64, "Low"), f(0x08, UInt64, "High")];
const UNICODE_STRING: &[CommonField] = &[
    f(0x00, UInt16, "Length"),
    f(0x02, UInt16, "MaximumLength"),
    f(0x04, Hex32, "_padding"),
    fp(0x08, Pointer64, "Buffer", "UTF16"),
];
const LIST_ENTRY: &[CommonField] = &[f(0x00, Pointer64, "Flink"), f(0x08, Pointer64, "Blink")];
const LARGE_INTEGER: &[CommonField] = &[f(0x00, UInt32, "LowPart"), f(0x04, Int32, "HighPart")];
const OBJECT_ATTRIBUTES: &[CommonField] = &[
    f(0x00, UInt32, "Length"),
    f(0x04, Hex32, "_pad"),
    f(0x08, Pointer64, "RootDirectory"),
    fp(0x10, Pointer64, "ObjectName", "UNICODE_STRING"),
    f(0x18, UInt32, "Attributes"),
    f(0x1c, Hex32, "_pad2"),
    f(0x20, Pointer64, "SecurityDescriptor"),
    f(0x28, Pointer64, "SecurityQualityOfService"),
];
const CLIENT_ID: &[CommonField] = &[
    f(0x00, Pointer64, "UniqueProcess"),
    f(0x08, Pointer64, "UniqueThread"),
];
const IO_STATUS_BLOCK: &[CommonField] = &[f(0x00, Int64, "Status"), f(0x08, UInt64, "Information")];
const GUID: &[CommonField] = &[
    f(0x00, UInt32, "Data1"),
    f(0x04, UInt16, "Data2"),
    f(0x06, UInt16, "Data3"),
    f(0x08, Hex64, "Data4"),
];
const FILETIME: &[CommonField] = &[
    f(0x00, UInt32, "dwLowDateTime"),
    f(0x04, UInt32, "dwHighDateTime"),
];
const FILETIME64: &[CommonField] = &[f(0x00, UInt64, "QuadPart")];
const UNIXTIME32: &[CommonField] = &[f(0x00, Int32, "seconds")];
const UNIXTIME64: &[CommonField] = &[f(0x00, Int64, "seconds")];
const RTL_BALANCED_NODE: &[CommonField] = &[
    f(0x00, Pointer64, "Left"),
    f(0x08, Pointer64, "Right"),
    f(0x10, UInt64, "ParentValue"),
];
const SINGLE_LIST_ENTRY: &[CommonField] = &[f(0x00, Pointer64, "Next")];
const STRING: &[CommonField] = &[
    f(0x00, UInt16, "Length"),
    f(0x02, UInt16, "MaximumLength"),
    f(0x04, Hex32, "_pad"),
    f(0x08, Pointer64, "Buffer"),
];
const DISPATCHER_HEADER: &[CommonField] = &[
    f(0x00, Int32, "Lock"),
    f(0x04, Int32, "SignalState"),
    f(0x08, Pointer64, "WaitListHead_Flink"),
    f(0x10, Pointer64, "WaitListHead_Blink"),
];

// ── C++ STL (MSVC x64) ──
const STD_STRING: &[CommonField] = &[
    f(0x00, Pointer64, "_Ptr"),
    f(0x08, Hex64, "_Buf_hi"),
    f(0x10, UInt64, "_Mysize"),
    f(0x18, UInt64, "_Myres"),
];
const STD_VECTOR: &[CommonField] = &[
    f(0x00, Pointer64, "_Myfirst"),
    f(0x08, Pointer64, "_Mylast"),
    f(0x10, Pointer64, "_Myend"),
];
const STD_SHARED_PTR: &[CommonField] = &[f(0x00, Pointer64, "_Ptr"), f(0x08, Pointer64, "_Rep")];
const STD_UNIQUE_PTR: &[CommonField] = &[f(0x00, Pointer64, "_Ptr")];
const STD_FUNCTION: &[CommonField] = &[
    f(0x00, Hex64, "_storage0"),
    f(0x08, Hex64, "_storage1"),
    f(0x10, Hex64, "_storage2"),
    f(0x18, Hex64, "_storage3"),
    f(0x20, Pointer64, "_impl"),
    f(0x28, Pointer64, "_invoke"),
];
const STD_MAP_NODE: &[CommonField] = &[
    f(0x00, Pointer64, "_Left"),
    f(0x08, Pointer64, "_Parent"),
    f(0x10, Pointer64, "_Right"),
    f(0x18, UInt8, "_Color"),
    f(0x19, UInt8, "_IsNil"),
    f(0x1a, Hex16, "_pad"),
    f(0x1c, Hex32, "_pad2"),
    f(0x20, Hex64, "_Key"),
    f(0x28, Hex64, "_Value"),
];
const STD_UNORDERED_MAP: &[CommonField] = &[
    f(0x00, Pointer64, "_List_head"),
    f(0x08, UInt64, "_List_size"),
    f(0x10, Pointer64, "_Vec_buckets"),
    f(0x18, UInt64, "_Vec_size"),
    f(0x20, UInt64, "_Mask"),
    f(0x28, UInt64, "_Maxidx"),
    f(0x30, Float, "_Max_load_factor"),
    f(0x34, Hex32, "_pad"),
];

// ── Unreal Engine ──
const FSTRING: &[CommonField] = &[
    f(0x00, Pointer64, "Data"),
    f(0x08, Int32, "Num"),
    f(0x0c, Int32, "Max"),
];
const FNAME: &[CommonField] = &[f(0x00, Int32, "ComparisonIndex"), f(0x04, Int32, "Number")];
const FVECTOR: &[CommonField] = &[
    f(0x00, Float, "X"),
    f(0x04, Float, "Y"),
    f(0x08, Float, "Z"),
];
const FROTATOR: &[CommonField] = &[
    f(0x00, Float, "Pitch"),
    f(0x04, Float, "Yaw"),
    f(0x08, Float, "Roll"),
];
const FTRANSFORM: &[CommonField] = &[
    f(0x00, Vec4, "Rotation"),
    f(0x10, Vec4, "Translation"),
    f(0x20, Vec4, "Scale3D"),
];
const FQUAT: &[CommonField] = &[
    f(0x00, Float, "X"),
    f(0x04, Float, "Y"),
    f(0x08, Float, "Z"),
    f(0x0c, Float, "W"),
];
const FLINEARCOLOR: &[CommonField] = &[
    f(0x00, Float, "R"),
    f(0x04, Float, "G"),
    f(0x08, Float, "B"),
    f(0x0c, Float, "A"),
];

// ── Generic patterns ──
const VTABLE: &[CommonField] = &[
    f(0x00, FuncPtr64, "fn0"),
    f(0x08, FuncPtr64, "fn1"),
    f(0x10, FuncPtr64, "fn2"),
    f(0x18, FuncPtr64, "fn3"),
    f(0x20, FuncPtr64, "fn4"),
    f(0x28, FuncPtr64, "fn5"),
    f(0x30, FuncPtr64, "fn6"),
    f(0x38, FuncPtr64, "fn7"),
];
const REF_COUNTED: &[CommonField] = &[
    f(0x00, Pointer64, "__vptr"),
    f(0x08, Int32, "_refCount"),
    f(0x0c, Int32, "_weakCount"),
];
const LINKED_NODE: &[CommonField] = &[
    f(0x00, Pointer64, "next"),
    f(0x08, Pointer64, "prev"),
    f(0x10, Pointer64, "data"),
];
const TREE_NODE: &[CommonField] = &[
    f(0x00, Pointer64, "left"),
    f(0x08, Pointer64, "right"),
    f(0x10, Pointer64, "parent"),
    f(0x18, Pointer64, "data"),
];
const SLAB_ENTRY: &[CommonField] = &[
    f(0x00, Pointer64, "data"),
    f(0x08, UInt32, "size"),
    f(0x0c, UInt32, "capacity"),
    f(0x10, UInt32, "flags"),
    f(0x14, UInt32, "refCount"),
];
const DELEGATE: &[CommonField] = &[f(0x00, Pointer64, "object"), f(0x08, Pointer64, "function")];
const VARIANT: &[CommonField] = &[
    f(0x00, Hex64, "data0"),
    f(0x08, Hex64, "data1"),
    f(0x10, UInt32, "typeId"),
    f(0x14, UInt32, "flags"),
];
const RGBA8: &[CommonField] = &[
    f(0x00, UInt8, "r"),
    f(0x01, UInt8, "g"),
    f(0x02, UInt8, "b"),
    f(0x03, UInt8, "a"),
];
const AABB: &[CommonField] = &[f(0x00, Vec3, "min"), f(0x0c, Vec3, "max")];
const MATRIX4X4: &[CommonField] = &[f(0x00, Mat4x4, "m")];
const SPHERE: &[CommonField] = &[f(0x00, Vec3, "center"), f(0x0c, Float, "radius")];
const RAY: &[CommonField] = &[f(0x00, Vec3, "origin"), f(0x0c, Vec3, "direction")];
const PLANE: &[CommonField] = &[f(0x00, Vec3, "normal"), f(0x0c, Float, "distance")];
const TIMESTAMP: &[CommonField] = &[f(0x00, Int64, "ticks")];
const SLICE: &[CommonField] = &[f(0x00, Pointer64, "ptr"), f(0x08, UInt64, "len")];
const FAT_POINTER: &[CommonField] = &[f(0x00, Pointer64, "ptr"), f(0x08, Pointer64, "vtable")];

const fn ct(
    name: &'static str,
    category: &'static str,
    class_keyword: &'static str,
    total_size: i32,
    fields: &'static [CommonField],
) -> CommonType {
    CommonType {
        name,
        category,
        class_keyword,
        total_size,
        fields,
    }
}

/// `kCommonTypes[]` (`commontypes.h:332-390`).
pub const K_COMMON_TYPES: &[CommonType] = &[
    // Windows NT
    ct("_M128A", "Windows NT", "struct", 16, M128A),
    ct("UNICODE_STRING", "Windows NT", "struct", 16, UNICODE_STRING),
    ct("LIST_ENTRY", "Windows NT", "struct", 16, LIST_ENTRY),
    ct("LARGE_INTEGER", "Windows NT", "union", 8, LARGE_INTEGER),
    ct(
        "OBJECT_ATTRIBUTES",
        "Windows NT",
        "struct",
        48,
        OBJECT_ATTRIBUTES,
    ),
    ct("CLIENT_ID", "Windows NT", "struct", 16, CLIENT_ID),
    ct(
        "IO_STATUS_BLOCK",
        "Windows NT",
        "struct",
        16,
        IO_STATUS_BLOCK,
    ),
    ct("GUID", "Windows NT", "struct", 16, GUID),
    ct("FILETIME", "Windows NT", "struct", 8, FILETIME),
    ct("FILETIME_u64", "Windows NT", "union", 8, FILETIME64),
    ct("UnixTime32", "Time", "struct", 4, UNIXTIME32),
    ct("UnixTime64", "Time", "struct", 8, UNIXTIME64),
    ct(
        "RTL_BALANCED_NODE",
        "Windows NT",
        "struct",
        24,
        RTL_BALANCED_NODE,
    ),
    ct(
        "SINGLE_LIST_ENTRY",
        "Windows NT",
        "struct",
        8,
        SINGLE_LIST_ENTRY,
    ),
    ct("STRING", "Windows NT", "struct", 16, STRING),
    ct(
        "DISPATCHER_HEADER",
        "Windows NT",
        "struct",
        24,
        DISPATCHER_HEADER,
    ),
    // C++ STL (MSVC x64)
    ct("std::string", "C++ STL", "class", 32, STD_STRING),
    ct("std::wstring", "C++ STL", "class", 32, STD_STRING),
    ct("std::vector", "C++ STL", "class", 24, STD_VECTOR),
    ct("std::shared_ptr", "C++ STL", "class", 16, STD_SHARED_PTR),
    ct("std::unique_ptr", "C++ STL", "class", 8, STD_UNIQUE_PTR),
    ct("std::function", "C++ STL", "class", 48, STD_FUNCTION),
    ct("std::map_node", "C++ STL", "struct", 48, STD_MAP_NODE),
    ct(
        "std::unordered_map",
        "C++ STL",
        "class",
        56,
        STD_UNORDERED_MAP,
    ),
    // Unreal Engine
    ct("FString", "Unreal", "struct", 16, FSTRING),
    ct("FName", "Unreal", "struct", 8, FNAME),
    ct("TArray", "Unreal", "struct", 16, FSTRING),
    ct("FVector", "Unreal", "struct", 12, FVECTOR),
    ct("FRotator", "Unreal", "struct", 12, FROTATOR),
    ct("FTransform", "Unreal", "struct", 48, FTRANSFORM),
    ct("FQuat", "Unreal", "struct", 16, FQUAT),
    ct("FLinearColor", "Unreal", "struct", 16, FLINEARCOLOR),
    // Generic patterns
    ct("VTable8", "Generic", "struct", 64, VTABLE),
    ct("RefCounted", "Generic", "class", 16, REF_COUNTED),
    ct("LinkedNode", "Generic", "struct", 24, LINKED_NODE),
    ct("TreeNode", "Generic", "struct", 32, TREE_NODE),
    ct("SlabEntry", "Generic", "struct", 24, SLAB_ENTRY),
    ct("Delegate", "Generic", "struct", 16, DELEGATE),
    ct("Variant", "Generic", "struct", 24, VARIANT),
    ct("Slice", "Generic", "struct", 16, SLICE),
    ct("FatPointer", "Generic", "struct", 16, FAT_POINTER),
    ct("TimeStamp", "Generic", "struct", 8, TIMESTAMP),
    // Math / Spatial
    ct("RGBA8", "Math", "struct", 4, RGBA8),
    ct("AABB", "Math", "struct", 24, AABB),
    ct("Matrix4x4", "Math", "struct", 64, MATRIX4X4),
    ct("Sphere", "Math", "struct", 16, SPHERE),
    ct("Ray", "Math", "struct", 24, RAY),
    ct("Plane", "Math", "struct", 16, PLANE),
];

/// `findCommonType()` (`commontypes.h:397-403`) — `None` if not found.
pub fn find_common_type(name: &str) -> Option<&'static CommonType> {
    K_COMMON_TYPES.iter().find(|t| t.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_and_field_counts() {
        let t = find_common_type("UNICODE_STRING").unwrap();
        assert_eq!(t.total_size, 16);
        assert_eq!(t.fields.len(), 4);
        assert_eq!(t.fields[3].ptr_target, "UTF16");
        assert!(find_common_type("nope").is_none());
        // The aliased types share a field array but report their own size.
        assert_eq!(find_common_type("std::wstring").unwrap().fields.len(), 4);
        assert_eq!(find_common_type("TArray").unwrap().fields.len(), 3);
    }
}
