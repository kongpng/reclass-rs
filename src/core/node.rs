//! `struct Node` (the field/record), `BitfieldMember`, and `Bookmark`.
//!
//! Faithful port of `src/core.h:200-382`. JSON (`to_json`/`from_json`) is
//! hand-mapped over `serde_json::Value` to reproduce the C++ byte-for-byte:
//! 64-bit ids/refId/parentId/enum-values are **decimal strings**, `collapsed`
//! always loads `true`, optional fields omitted when default, `isStatic`
//! falls back to the legacy `isHelper` key.

use serde_json::{json, Map, Value};

use super::kind::{kind_from_string, kind_to_string, size_for_kind, NodeKind};

/// `static constexpr int kMaxArrayLen = 1000000` (`core.h:198`).
pub const K_MAX_ARRAY_LEN: i32 = 1_000_000;

/// `struct BitfieldMember` (`core.h:202-206`).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct BitfieldMember {
    pub name: String,
    /// position from LSB within the container.
    pub bit_offset: u8,
    /// number of bits (1..64).
    pub bit_width: u8,
}

/// `struct Node` (`core.h:210-363`).
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// unique node id; `0` = unassigned (`add_node` auto-assigns).
    pub id: u64,
    pub kind: NodeKind,
    pub name: String,
    /// Struct/Array: optional named type (e.g. "IMAGE_DOS_HEADER").
    pub struct_type_name: String,
    /// "struct"/"class"/"union"/"enum"/"bitfield"; empty == "struct".
    pub class_keyword: String,
    /// parent node id; `0` = root.
    pub parent_id: u64,
    pub offset: i32,
    /// static field — excluded from struct layout/span.
    pub is_static: bool,
    /// C/C++ expression → absolute address (static fields only).
    pub offset_expr: String,
    /// Pointer: target = base + value (RVA) vs absolute.
    pub is_relative: bool,
    /// Array element count.
    pub array_len: i32,
    /// string length (UTF8/UTF16).
    pub str_len: i32,
    /// UI fold state.
    pub collapsed: bool,
    /// Pointer32/64: id of Struct to expand at `*ptr`; also embedded-struct span.
    pub ref_id: u64,
    /// Array element type; for Pointer with `ptr_depth>0`: primitive target type.
    pub element_kind: NodeKind,
    /// Pointer: 0=struct/void*, 1=primitive*, 2=primitive**.
    pub ptr_depth: i32,
    /// Array: transient current view offset (NOT serialized).
    pub view_index: i32,
    /// Enum: name→value pairs.
    pub enum_members: Vec<(String, i64)>,
    /// Bitfield: per-bit member defs.
    pub bitfield_members: Vec<BitfieldMember>,
    /// user annotation (rendered "// text").
    pub comment: String,
    /// scalar is big-endian (swap on display/parse).
    pub big_endian: bool,
}

impl Default for Node {
    fn default() -> Self {
        Node {
            id: 0,
            kind: NodeKind::Hex8,
            name: String::new(),
            struct_type_name: String::new(),
            class_keyword: String::new(),
            parent_id: 0,
            offset: 0,
            is_static: false,
            offset_expr: String::new(),
            is_relative: false,
            array_len: 1,
            str_len: 64,
            collapsed: true,
            ref_id: 0,
            element_kind: NodeKind::UInt8,
            ptr_depth: 0,
            view_index: 0,
            enum_members: Vec::new(),
            bitfield_members: Vec::new(),
            comment: String::new(),
            big_endian: false,
        }
    }
}

#[inline]
fn qbound(lo: i32, v: i32, hi: i32) -> i32 {
    v.clamp(lo, hi)
}

/// Lenient unsigned 64-bit parse from a JSON string field (decimal), mirroring
/// `QString::toULongLong()` (returns 0 on failure).
fn str_to_u64(v: &Value, default: &str) -> u64 {
    let s = v.as_str().unwrap_or(default);
    s.trim().parse::<u64>().unwrap_or(0)
}

fn str_to_i64(v: &Value, default: &str) -> i64 {
    let s = v.as_str().unwrap_or(default);
    s.trim().parse::<i64>().unwrap_or(0)
}

impl Node {
    /// `int Node::byteSize() const` (`core.h:238-255`) — leaf size only.
    pub fn byte_size(&self) -> i32 {
        match self.kind {
            NodeKind::UTF8 => self.str_len,
            NodeKind::UTF16 => self.str_len.min(i32::MAX / 2) * 2,
            NodeKind::Array => {
                let elem_sz = size_for_kind(self.element_kind);
                if elem_sz <= 0 {
                    0
                } else {
                    self.array_len.min(i32::MAX / elem_sz) * elem_sz
                }
            }
            NodeKind::Struct => {
                if self.class_keyword == "bitfield" {
                    let sz = size_for_kind(self.element_kind);
                    if sz > 0 {
                        sz
                    } else {
                        4
                    }
                } else {
                    0
                }
            }
            _ => size_for_kind(self.kind),
        }
    }

    /// `Node::toJson()` (`core.h:263-313`).
    pub fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("id".into(), json!(self.id.to_string()));
        o.insert("kind".into(), json!(kind_to_string(self.kind)));
        o.insert("name".into(), json!(self.name));
        if !self.struct_type_name.is_empty() {
            o.insert("structTypeName".into(), json!(self.struct_type_name));
        }
        if !self.class_keyword.is_empty() && self.class_keyword != "struct" {
            o.insert("classKeyword".into(), json!(self.class_keyword));
        }
        o.insert("parentId".into(), json!(self.parent_id.to_string()));
        o.insert("offset".into(), json!(self.offset));
        if self.is_static {
            o.insert("isStatic".into(), json!(true));
        }
        if !self.offset_expr.is_empty() {
            o.insert("offsetExpr".into(), json!(self.offset_expr));
        }
        if self.is_relative {
            o.insert("isRelative".into(), json!(true));
        }
        o.insert("arrayLen".into(), json!(self.array_len));
        o.insert("strLen".into(), json!(self.str_len));
        o.insert("collapsed".into(), json!(self.collapsed));
        o.insert("refId".into(), json!(self.ref_id.to_string()));
        o.insert(
            "elementKind".into(),
            json!(kind_to_string(self.element_kind)),
        );
        if self.ptr_depth > 0 {
            o.insert("ptrDepth".into(), json!(self.ptr_depth));
        }
        if !self.enum_members.is_empty() {
            let arr: Vec<Value> = self
                .enum_members
                .iter()
                .map(|(name, value)| json!({ "name": name, "value": value.to_string() }))
                .collect();
            o.insert("enumMembers".into(), Value::Array(arr));
        }
        if !self.bitfield_members.is_empty() {
            let arr: Vec<Value> = self
                .bitfield_members
                .iter()
                .map(|m| {
                    json!({
                        "name": m.name,
                        "bitOffset": m.bit_offset as i32,
                        "bitWidth": m.bit_width as i32,
                    })
                })
                .collect();
            o.insert("bitfieldMembers".into(), Value::Array(arr));
        }
        if !self.comment.is_empty() {
            o.insert("comment".into(), json!(self.comment));
        }
        if self.big_endian {
            o.insert("bigEndian".into(), json!(true));
        }
        Value::Object(o)
    }

    /// `static Node Node::fromJson()` (`core.h:314-354`).
    pub fn from_json(o: &Value) -> Node {
        let get = |k: &str| o.get(k).cloned().unwrap_or(Value::Null);
        let mut n = Node {
            id: str_to_u64(&get("id"), "0"),
            kind: kind_from_string(get("kind").as_str().unwrap_or("")),
            name: get("name").as_str().unwrap_or("").to_string(),
            struct_type_name: get("structTypeName").as_str().unwrap_or("").to_string(),
            class_keyword: get("classKeyword").as_str().unwrap_or("").to_string(),
            parent_id: str_to_u64(&get("parentId"), "0"),
            offset: get("offset").as_i64().unwrap_or(0) as i32,
            // backward-compat with the legacy `isHelper` key.
            is_static: get("isStatic")
                .as_bool()
                .unwrap_or_else(|| get("isHelper").as_bool().unwrap_or(false)),
            offset_expr: get("offsetExpr").as_str().unwrap_or("").to_string(),
            is_relative: get("isRelative").as_bool().unwrap_or(false),
            array_len: qbound(
                1,
                get("arrayLen").as_i64().unwrap_or(1) as i32,
                K_MAX_ARRAY_LEN,
            ),
            str_len: qbound(1, get("strLen").as_i64().unwrap_or(64) as i32, 1_000_000),
            collapsed: true, // Always load collapsed; user expands as needed.
            ref_id: str_to_u64(&get("refId"), "0"),
            element_kind: kind_from_string(get("elementKind").as_str().unwrap_or("UInt8")),
            ptr_depth: qbound(0, get("ptrDepth").as_i64().unwrap_or(0) as i32, 2),
            ..Node::default()
        };
        if let Some(arr) = get("enumMembers").as_array() {
            for v in arr {
                let name = v
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let value = str_to_i64(&v.get("value").cloned().unwrap_or(Value::Null), "0");
                n.enum_members.push((name, value));
            }
        }
        if let Some(arr) = get("bitfieldMembers").as_array() {
            for v in arr {
                n.bitfield_members.push(BitfieldMember {
                    name: v
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    bit_offset: qbound(
                        0,
                        v.get("bitOffset").and_then(Value::as_i64).unwrap_or(0) as i32,
                        255,
                    ) as u8,
                    bit_width: qbound(
                        1,
                        v.get("bitWidth").and_then(Value::as_i64).unwrap_or(1) as i32,
                        64,
                    ) as u8,
                });
            }
        }
        n.comment = get("comment").as_str().unwrap_or("").to_string();
        n.big_endian = get("bigEndian").as_bool().unwrap_or(false);
        n
    }

    /// `resolvedClassKeyword()` (`core.h:357`) — never empty.
    pub fn resolved_class_keyword(&self) -> &str {
        if self.class_keyword.is_empty() {
            "struct"
        } else {
            &self.class_keyword
        }
    }
    pub fn is_union(&self) -> bool {
        self.resolved_class_keyword() == "union"
    }
    /// Note: checks the raw keyword, not the resolved one — empty is NOT bitfield.
    pub fn is_bitfield(&self) -> bool {
        self.class_keyword == "bitfield"
    }
    pub fn is_enum(&self) -> bool {
        self.resolved_class_keyword() == "enum"
    }
}

/// `struct Bookmark` (`core.h:367-382`).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Bookmark {
    pub name: String,
    /// e.g. "<game.exe>+0x12340" — survives rebases.
    pub address_formula: String,
}

impl Bookmark {
    pub fn to_json(&self) -> Value {
        json!({ "name": self.name, "addressFormula": self.address_formula })
    }
    pub fn from_json(o: &Value) -> Bookmark {
        Bookmark {
            name: o
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            address_formula: o
                .get("addressFormula")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_size_leaf_and_container() {
        let mut n = Node {
            kind: NodeKind::UTF16,
            str_len: 10,
            ..Node::default()
        };
        assert_eq!(n.byte_size(), 20);
        n.kind = NodeKind::Array;
        n.element_kind = NodeKind::UInt32;
        n.array_len = 16;
        assert_eq!(n.byte_size(), 64);
        n.kind = NodeKind::Struct;
        assert_eq!(n.byte_size(), 0); // needs tree context
    }

    #[test]
    fn json_round_trips_all_optional_fields() {
        let n = Node {
            id: 0xDEAD_BEEF_CAFE,
            kind: NodeKind::Pointer64,
            name: "ptr".into(),
            struct_type_name: "FOO".into(),
            class_keyword: "union".into(),
            parent_id: 7,
            offset: 0x7FFF_FFFF,
            is_static: true,
            offset_expr: "<a.exe>+0x10".into(),
            is_relative: true,
            array_len: 3,
            str_len: 64,
            ref_id: 42,
            element_kind: NodeKind::Int32,
            ptr_depth: 2,
            enum_members: vec![("A".into(), -1), ("B".into(), 9_000_000_000)],
            bitfield_members: vec![BitfieldMember {
                name: "flag".into(),
                bit_offset: 3,
                bit_width: 5,
            }],
            comment: "hi".into(),
            big_endian: true,
            ..Node::default()
        };
        let back = Node::from_json(&n.to_json());
        // collapsed always loads true; everything else must match.
        assert_eq!(back.collapsed, true);
        let mut expect = n.clone();
        expect.collapsed = true;
        expect.view_index = 0;
        assert_eq!(back, expect);
    }

    #[test]
    fn is_helper_legacy_key() {
        let v = json!({ "id": "1", "kind": "Hex8", "isHelper": true });
        assert!(Node::from_json(&v).is_static);
    }
}
