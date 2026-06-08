//! Node kind enum + the `KindMeta` metadata table — the single source of truth
//! for size/alignment/display-name/line-count/flags.
//!
//! Faithful port of `src/core.h:24-178` (`enum class NodeKind`, `KindFlags`,
//! `struct KindMeta`, `kKindMeta[]`, and the free functions over kinds).

/// `enum class NodeKind : uint8_t` (`core.h:24-34`).
///
/// The integer discriminant is **load-bearing**: it indexes [`K_KIND_META`].
/// Order MUST be preserved (`Array` is last; the table has exactly 31 entries).
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeKind {
    Hex8 = 0,
    Hex16,
    Hex32,
    Hex64,
    Hex128,
    Int8,
    Int16,
    Int32,
    Int64,
    Int128,
    UInt8,
    UInt16,
    UInt32,
    UInt64,
    UInt128,
    Float16,
    Float,
    Double,
    Bool,
    Pointer32,
    Pointer64,
    FuncPtr32,
    FuncPtr64,
    Vec2,
    Vec3,
    Vec4,
    Mat4x4,
    UTF8,
    UTF16,
    Struct,
    Array,
}

impl Default for NodeKind {
    fn default() -> Self {
        NodeKind::Hex8
    }
}

/// `enum KindFlags : uint32_t` (`core.h:44-50`).
pub mod flags {
    pub const KF_NONE: u32 = 0;
    pub const KF_HEX_PREVIEW: u32 = 1 << 0; // Hex8..Hex128
    pub const KF_CONTAINER: u32 = 1 << 1; // Struct/Array
    pub const KF_STRING: u32 = 1 << 2; // UTF8/UTF16
    pub const KF_VECTOR: u32 = 1 << 3; // Vec2/3/4
}

/// `struct KindMeta` (`core.h:54-62`).
#[derive(Copy, Clone, Debug)]
pub struct KindMeta {
    pub kind: NodeKind,
    /// UI/JSON name: "Hex64", "UInt16". Used for JSON round-trip.
    pub name: &'static str,
    /// display / C name: "hex64", "uint16_t", "ptr64", "str". Used by
    /// `kind_from_type_name` and the type-name UI.
    pub type_name: &'static str,
    /// byte size (0 = dynamic: Struct/Array).
    pub size: i32,
    /// display line count (1 for everything except Mat4x4 = 4).
    pub lines: i32,
    /// natural alignment.
    pub align: i32,
    /// `KindFlags` bitmask.
    pub flags: u32,
}

/// `kKindMeta[]` (`core.h:64-97`). Exactly 31 entries, indexed by `kind as usize`.
pub const K_KIND_META: [KindMeta; 31] = {
    use flags::*;
    macro_rules! m {
        ($k:ident, $n:literal, $t:literal, $sz:literal, $ln:literal, $al:literal, $fl:expr) => {
            KindMeta {
                kind: NodeKind::$k,
                name: $n,
                type_name: $t,
                size: $sz,
                lines: $ln,
                align: $al,
                flags: $fl,
            }
        };
    }
    [
        m!(Hex8, "Hex8", "hex8", 1, 1, 1, KF_HEX_PREVIEW),
        m!(Hex16, "Hex16", "hex16", 2, 1, 2, KF_HEX_PREVIEW),
        m!(Hex32, "Hex32", "hex32", 4, 1, 4, KF_HEX_PREVIEW),
        m!(Hex64, "Hex64", "hex64", 8, 1, 8, KF_HEX_PREVIEW),
        m!(Hex128, "Hex128", "hex128", 16, 1, 16, KF_HEX_PREVIEW),
        m!(Int8, "Int8", "int8_t", 1, 1, 1, KF_NONE),
        m!(Int16, "Int16", "int16_t", 2, 1, 2, KF_NONE),
        m!(Int32, "Int32", "int32_t", 4, 1, 4, KF_NONE),
        m!(Int64, "Int64", "int64_t", 8, 1, 8, KF_NONE),
        m!(Int128, "Int128", "int128_t", 16, 1, 8, KF_NONE),
        m!(UInt8, "UInt8", "uint8_t", 1, 1, 1, KF_NONE),
        m!(UInt16, "UInt16", "uint16_t", 2, 1, 2, KF_NONE),
        m!(UInt32, "UInt32", "uint32_t", 4, 1, 4, KF_NONE),
        m!(UInt64, "UInt64", "uint64_t", 8, 1, 8, KF_NONE),
        m!(UInt128, "UInt128", "uint128_t", 16, 1, 8, KF_NONE),
        m!(Float16, "Float16", "float16", 2, 1, 2, KF_NONE),
        m!(Float, "Float", "float", 4, 1, 4, KF_NONE),
        m!(Double, "Double", "double", 8, 1, 8, KF_NONE),
        m!(Bool, "Bool", "bool", 1, 1, 1, KF_NONE),
        m!(Pointer32, "Pointer32", "ptr32", 4, 1, 4, KF_NONE),
        m!(Pointer64, "Pointer64", "ptr64", 8, 1, 8, KF_NONE),
        m!(FuncPtr32, "FuncPtr32", "fnptr32", 4, 1, 4, KF_NONE),
        m!(FuncPtr64, "FuncPtr64", "fnptr64", 8, 1, 8, KF_NONE),
        m!(Vec2, "Vec2", "vec2", 8, 1, 4, KF_VECTOR),
        m!(Vec3, "Vec3", "vec3", 12, 1, 4, KF_VECTOR),
        m!(Vec4, "Vec4", "vec4", 16, 1, 4, KF_VECTOR),
        m!(Mat4x4, "Mat4x4", "mat4x4", 64, 4, 4, KF_NONE),
        m!(UTF8, "UTF8", "str", 1, 1, 1, KF_STRING),
        m!(UTF16, "UTF16", "wstr", 2, 1, 2, KF_STRING),
        m!(Struct, "Struct", "struct", 0, 1, 1, KF_CONTAINER),
        m!(Array, "Array", "array", 0, 1, 1, KF_CONTAINER),
    ]
};

/// `kindMeta(NodeKind)` (`core.h:102-106`). `None` only for an out-of-range
/// value, which a closed `enum` cannot produce — kept for parity with the C++
/// `nullptr` contract that callers defensively handle.
#[inline]
pub fn kind_meta(k: NodeKind) -> Option<&'static KindMeta> {
    K_KIND_META.get(k as usize)
}

/// `sizeForKind` (`core.h:108`).
#[inline]
pub fn size_for_kind(k: NodeKind) -> i32 {
    kind_meta(k).map_or(0, |m| m.size)
}

/// `linesForKind` (`core.h:109`) — default **1**, not 0.
#[inline]
pub fn lines_for_kind(k: NodeKind) -> i32 {
    kind_meta(k).map_or(1, |m| m.lines)
}

/// `alignmentFor` (`core.h:110`).
#[inline]
pub fn alignment_for(k: NodeKind) -> i32 {
    kind_meta(k).map_or(1, |m| m.align)
}

/// `kindToString` (`core.h:112-115`).
#[inline]
pub fn kind_to_string(k: NodeKind) -> &'static str {
    kind_meta(k).map_or("Unknown", |m| m.name)
}

/// `kindFromString` (`core.h:117-121`) — unknown ⇒ `Hex8`.
pub fn kind_from_string(s: &str) -> NodeKind {
    for m in &K_KIND_META {
        if s == m.name {
            return m.kind;
        }
    }
    NodeKind::Hex8
}

/// `kindFromTypeName` (`core.h:123-132`) — returns `(kind, ok)`; miss ⇒ `(Hex8, false)`.
pub fn kind_from_type_name(s: &str) -> (NodeKind, bool) {
    for m in &K_KIND_META {
        if s == m.type_name {
            return (m.kind, true);
        }
    }
    (NodeKind::Hex8, false)
}

/// `flagsFor` (`core.h:134-137`).
#[inline]
pub fn flags_for(k: NodeKind) -> u32 {
    kind_meta(k).map_or(0, |m| m.flags)
}

#[inline]
pub fn is_hex_node(k: NodeKind) -> bool {
    k >= NodeKind::Hex8 && k <= NodeKind::Hex128
}
#[inline]
pub fn is_hex_preview(k: NodeKind) -> bool {
    is_hex_node(k)
}
#[inline]
pub fn is_vector_kind(k: NodeKind) -> bool {
    matches!(k, NodeKind::Vec2 | NodeKind::Vec3 | NodeKind::Vec4)
}
#[inline]
pub fn is_matrix_kind(k: NodeKind) -> bool {
    k == NodeKind::Mat4x4
}
#[inline]
pub fn is_func_ptr(k: NodeKind) -> bool {
    matches!(k, NodeKind::FuncPtr32 | NodeKind::FuncPtr64)
}
#[inline]
pub fn is_pointer_kind(k: NodeKind) -> bool {
    matches!(k, NodeKind::Pointer32 | NodeKind::Pointer64)
}
#[inline]
pub fn is_container_kind(k: NodeKind) -> bool {
    matches!(k, NodeKind::Struct | NodeKind::Array)
}
#[inline]
pub fn is_string_kind(k: NodeKind) -> bool {
    matches!(k, NodeKind::UTF8 | NodeKind::UTF16)
}

/// The same-byte-size "variant ring" for the ←/→ type cycler and the statusbar
/// pos/total indicator (the C++ cycle filter): every fixed-size kind of the SAME
/// byte size as `kind`, in `K_KIND_META` table order, EXCLUDING containers and —
/// unless `kind` is itself one — string (UTF8/UTF16) and vector (Vec2/3/4) kinds.
/// A dynamic/0-byte kind, or a kind with no same-size peers, returns just `[kind]`.
/// Single source of the filter shared by the editor cycler and the status bar.
#[cfg(feature = "ui")]
pub(crate) fn same_size_variants(kind: NodeKind) -> Vec<NodeKind> {
    let size = size_for_kind(kind);
    if size <= 0 {
        return vec![kind];
    }
    let cur_is_string = is_string_kind(kind);
    let cur_is_vector = is_vector_kind(kind);
    let ring: Vec<NodeKind> = K_KIND_META
        .iter()
        .map(|m| m.kind)
        .filter(|&k| {
            !is_container_kind(k)
                && size_for_kind(k) == size
                && (cur_is_string || !is_string_kind(k))
                && (cur_is_vector || !is_vector_kind(k))
        })
        .collect();
    if ring.is_empty() {
        vec![kind]
    } else {
        ring
    }
}

/// `isValidPrimitivePtrTarget` (`core.h:164-170`).
pub fn is_valid_primitive_ptr_target(k: NodeKind) -> bool {
    if is_hex_node(k) || is_pointer_kind(k) || is_func_ptr(k) {
        return false;
    }
    !matches!(k, NodeKind::Struct | NodeKind::Array)
}

/// `allTypeNamesForUI` (`core.h:172-178`) — every `type_name` in table order.
/// The `strip_brackets` parameter is ignored, exactly as in the C++.
pub fn all_type_names_for_ui(_strip_brackets: bool) -> Vec<&'static str> {
    K_KIND_META.iter().map(|m| m.type_name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_31_entries_aligned_to_enum() {
        assert_eq!(K_KIND_META.len(), 31);
        assert_eq!(NodeKind::Array as usize, 30);
        // every entry indexes back to itself
        for (i, m) in K_KIND_META.iter().enumerate() {
            assert_eq!(m.kind as usize, i);
            assert!(!m.name.is_empty());
            assert!(!m.type_name.is_empty());
            assert!(m.lines >= 1);
            assert!(m.align >= 1);
        }
    }

    #[test]
    fn kind_string_round_trips() {
        for m in &K_KIND_META {
            assert_eq!(kind_from_string(kind_to_string(m.kind)), m.kind);
            assert_eq!(kind_from_type_name(m.type_name), (m.kind, true));
        }
        // unknown ⇒ Hex8
        assert_eq!(kind_from_string("Nope"), NodeKind::Hex8);
        assert_eq!(kind_from_type_name("nope"), (NodeKind::Hex8, false));
    }

    #[test]
    fn specific_sizes_and_aligns() {
        assert_eq!(size_for_kind(NodeKind::Vec3), 12);
        assert_eq!(size_for_kind(NodeKind::Mat4x4), 64);
        assert_eq!(lines_for_kind(NodeKind::Mat4x4), 4);
        assert_eq!(size_for_kind(NodeKind::Hex128), 16);
        assert_eq!(alignment_for(NodeKind::Hex128), 16);
        assert_eq!(alignment_for(NodeKind::Int128), 8); // size 16 but align 8
        assert_eq!(size_for_kind(NodeKind::Struct), 0);
        assert_eq!(alignment_for(NodeKind::Struct), 1);
    }
}
