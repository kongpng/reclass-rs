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

// ─────────────────────────────────────────────────────────────────────────────
// Evidence records (`core.h:398-572`)
// ─────────────────────────────────────────────────────────────────────────────
//
// The NodeTree carries three evidence arrays (events / hypotheses / proposals)
// that record the provenance of reverse-engineering decisions. They are
// JSON-serialized field-for-field to match the C++ exactly, including the
// omit-when-empty / omit-when-default rules in each `toJson()`.

/// Parse the trailing decimal sequence of a prefixed id (`ev_42` → `42`).
/// Mirrors `parsePrefixedSequence` (`core.h:391-396`): returns 0 unless the id
/// starts with `prefix` and the remainder parses cleanly as an unsigned int.
pub fn parse_prefixed_sequence(id: &str, prefix: &str) -> u64 {
    match id.strip_prefix(prefix) {
        Some(rest) => rest.parse::<u64>().unwrap_or(0),
        None => 0,
    }
}

/// `struct EvidenceEvent` (`core.h:398-456`).
#[derive(Clone, Debug, PartialEq)]
pub struct EvidenceEvent {
    /// `ev_N`.
    pub id: String,
    /// msec since epoch.
    pub timestamp: i64,
    pub source: String,
    pub kind: String,
    pub summary: String,
    pub type_name: String,
    pub node_id: u64,
    /// offset within type, -1 = not field-specific.
    pub field_offset: i32,
    pub address: String,
    pub function_name: String,
    pub function_address: String,
    pub instruction: String,
    /// -1.0 = unset.
    pub confidence: f64,
    pub tags: Vec<String>,
    /// source-specific structured payload (a JSON object, or `Null` when empty).
    pub data: Value,
}

impl Default for EvidenceEvent {
    /// C++ aggregate defaults: `fieldOffset = -1`, `confidence = -1.0`,
    /// everything else zero/empty.
    fn default() -> Self {
        EvidenceEvent {
            id: String::new(),
            timestamp: 0,
            source: String::new(),
            kind: String::new(),
            summary: String::new(),
            type_name: String::new(),
            node_id: 0,
            field_offset: -1,
            address: String::new(),
            function_name: String::new(),
            function_address: String::new(),
            instruction: String::new(),
            confidence: -1.0,
            tags: Vec::new(),
            data: Value::Null,
        }
    }
}

impl EvidenceEvent {
    /// `EvidenceEvent::toJson()` (`core.h:415-433`).
    pub fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("id".into(), json!(self.id));
        o.insert("timestamp".into(), json!(self.timestamp.to_string()));
        if !self.source.is_empty() {
            o.insert("source".into(), json!(self.source));
        }
        if !self.kind.is_empty() {
            o.insert("kind".into(), json!(self.kind));
        }
        if !self.summary.is_empty() {
            o.insert("summary".into(), json!(self.summary));
        }
        if !self.type_name.is_empty() {
            o.insert("typeName".into(), json!(self.type_name));
        }
        if self.node_id != 0 {
            o.insert("nodeId".into(), json!(self.node_id.to_string()));
        }
        if self.field_offset >= 0 {
            o.insert("fieldOffset".into(), json!(self.field_offset));
        }
        if !self.address.is_empty() {
            o.insert("address".into(), json!(self.address));
        }
        if !self.function_name.is_empty() {
            o.insert("functionName".into(), json!(self.function_name));
        }
        if !self.function_address.is_empty() {
            o.insert("functionAddress".into(), json!(self.function_address));
        }
        if !self.instruction.is_empty() {
            o.insert("instruction".into(), json!(self.instruction));
        }
        if self.confidence >= 0.0 {
            o.insert("confidence".into(), json!(self.confidence));
        }
        if !self.tags.is_empty() {
            o.insert("tags".into(), json!(self.tags));
        }
        if json_object_non_empty(&self.data) {
            o.insert("data".into(), self.data.clone());
        }
        Value::Object(o)
    }

    /// `EvidenceEvent::fromJson()` (`core.h:435-455`).
    pub fn from_json(o: &Value) -> EvidenceEvent {
        EvidenceEvent {
            id: str_field(o, "id"),
            timestamp: str_to_i64(&o.get("timestamp").cloned().unwrap_or(Value::Null), "0"),
            source: str_field(o, "source"),
            kind: str_field(o, "kind"),
            summary: str_field(o, "summary"),
            type_name: str_field(o, "typeName"),
            node_id: str_to_u64(&o.get("nodeId").cloned().unwrap_or(Value::Null), "0"),
            field_offset: if o.get("fieldOffset").is_some() {
                o.get("fieldOffset").and_then(Value::as_i64).unwrap_or(-1) as i32
            } else {
                -1
            },
            address: str_field(o, "address"),
            function_name: str_field(o, "functionName"),
            function_address: str_field(o, "functionAddress"),
            instruction: str_field(o, "instruction"),
            confidence: if o.get("confidence").is_some() {
                o.get("confidence").and_then(Value::as_f64).unwrap_or(-1.0)
            } else {
                -1.0
            },
            tags: str_array(o, "tags"),
            data: object_or_null(o, "data"),
        }
    }
}

/// `struct EvidenceHypothesis` (`core.h:458-516`).
#[derive(Clone, Debug, PartialEq)]
pub struct EvidenceHypothesis {
    /// `hyp_N`.
    pub id: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// open, confirmed, rejected.
    pub status: String,
    pub claim: String,
    pub label: String,
    pub type_name: String,
    pub node_id: u64,
    pub field_offset: i32,
    pub confidence: f64,
    pub supporting_evidence_ids: Vec<String>,
    pub contradicting_evidence_ids: Vec<String>,
    pub recommended_validation: Vec<String>,
    pub notes: String,
    pub data: Value,
}

impl Default for EvidenceHypothesis {
    fn default() -> Self {
        EvidenceHypothesis {
            id: String::new(),
            created_at: 0,
            updated_at: 0,
            status: "open".to_string(),
            claim: String::new(),
            label: String::new(),
            type_name: String::new(),
            node_id: 0,
            field_offset: -1,
            confidence: 0.0,
            supporting_evidence_ids: Vec::new(),
            contradicting_evidence_ids: Vec::new(),
            recommended_validation: Vec::new(),
            notes: String::new(),
            data: Value::Null,
        }
    }
}

impl EvidenceHypothesis {
    /// `EvidenceHypothesis::toJson()` (`core.h:474-496`).
    pub fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("id".into(), json!(self.id));
        o.insert("createdAt".into(), json!(self.created_at.to_string()));
        o.insert("updatedAt".into(), json!(self.updated_at.to_string()));
        o.insert("status".into(), json!(self.status));
        if !self.claim.is_empty() {
            o.insert("claim".into(), json!(self.claim));
        }
        if !self.label.is_empty() {
            o.insert("label".into(), json!(self.label));
        }
        if !self.type_name.is_empty() {
            o.insert("typeName".into(), json!(self.type_name));
        }
        if self.node_id != 0 {
            o.insert("nodeId".into(), json!(self.node_id.to_string()));
        }
        if self.field_offset >= 0 {
            o.insert("fieldOffset".into(), json!(self.field_offset));
        }
        o.insert("confidence".into(), json!(self.confidence));
        if !self.supporting_evidence_ids.is_empty() {
            o.insert(
                "supportingEvidenceIds".into(),
                json!(self.supporting_evidence_ids),
            );
        }
        if !self.contradicting_evidence_ids.is_empty() {
            o.insert(
                "contradictingEvidenceIds".into(),
                json!(self.contradicting_evidence_ids),
            );
        }
        if !self.recommended_validation.is_empty() {
            o.insert(
                "recommendedValidation".into(),
                json!(self.recommended_validation),
            );
        }
        if !self.notes.is_empty() {
            o.insert("notes".into(), json!(self.notes));
        }
        if json_object_non_empty(&self.data) {
            o.insert("data".into(), self.data.clone());
        }
        Value::Object(o)
    }

    /// `EvidenceHypothesis::fromJson()` (`core.h:497-515`).
    pub fn from_json(o: &Value) -> EvidenceHypothesis {
        EvidenceHypothesis {
            id: str_field(o, "id"),
            created_at: str_to_i64(&o.get("createdAt").cloned().unwrap_or(Value::Null), "0"),
            updated_at: str_to_i64(&o.get("updatedAt").cloned().unwrap_or(Value::Null), "0"),
            status: str_field_default(o, "status", "open"),
            claim: str_field(o, "claim"),
            label: str_field(o, "label"),
            type_name: str_field(o, "typeName"),
            node_id: str_to_u64(&o.get("nodeId").cloned().unwrap_or(Value::Null), "0"),
            field_offset: if o.get("fieldOffset").is_some() {
                o.get("fieldOffset").and_then(Value::as_i64).unwrap_or(-1) as i32
            } else {
                -1
            },
            confidence: o.get("confidence").and_then(Value::as_f64).unwrap_or(0.0),
            supporting_evidence_ids: str_array(o, "supportingEvidenceIds"),
            contradicting_evidence_ids: str_array(o, "contradictingEvidenceIds"),
            recommended_validation: str_array(o, "recommendedValidation"),
            notes: str_field(o, "notes"),
            data: object_or_null(o, "data"),
        }
    }
}

/// `struct EvidenceProposal` (`core.h:518-572`).
#[derive(Clone, Debug, PartialEq)]
pub struct EvidenceProposal {
    /// `prop_N`.
    pub id: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// pending, accepted, rejected, applied.
    pub status: String,
    pub title: String,
    pub action: String,
    pub type_name: String,
    pub node_id: u64,
    pub field_offset: i32,
    pub confidence: f64,
    pub evidence_ids: Vec<String>,
    /// usually tree.apply operations (a JSON array, or `Null` when empty).
    pub operations: Value,
    pub data: Value,
}

impl Default for EvidenceProposal {
    fn default() -> Self {
        EvidenceProposal {
            id: String::new(),
            created_at: 0,
            updated_at: 0,
            status: "pending".to_string(),
            title: String::new(),
            action: String::new(),
            type_name: String::new(),
            node_id: 0,
            field_offset: -1,
            confidence: 0.0,
            evidence_ids: Vec::new(),
            operations: Value::Null,
            data: Value::Null,
        }
    }
}

impl EvidenceProposal {
    /// `EvidenceProposal::toJson()` (`core.h:536-553`).
    pub fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("id".into(), json!(self.id));
        o.insert("createdAt".into(), json!(self.created_at.to_string()));
        o.insert("updatedAt".into(), json!(self.updated_at.to_string()));
        o.insert("status".into(), json!(self.status));
        if !self.title.is_empty() {
            o.insert("title".into(), json!(self.title));
        }
        if !self.action.is_empty() {
            o.insert("action".into(), json!(self.action));
        }
        if !self.type_name.is_empty() {
            o.insert("typeName".into(), json!(self.type_name));
        }
        if self.node_id != 0 {
            o.insert("nodeId".into(), json!(self.node_id.to_string()));
        }
        if self.field_offset >= 0 {
            o.insert("fieldOffset".into(), json!(self.field_offset));
        }
        o.insert("confidence".into(), json!(self.confidence));
        if !self.evidence_ids.is_empty() {
            o.insert("evidenceIds".into(), json!(self.evidence_ids));
        }
        if json_array_non_empty(&self.operations) {
            o.insert("operations".into(), self.operations.clone());
        }
        if json_object_non_empty(&self.data) {
            o.insert("data".into(), self.data.clone());
        }
        Value::Object(o)
    }

    /// `EvidenceProposal::fromJson()` (`core.h:554-571`).
    pub fn from_json(o: &Value) -> EvidenceProposal {
        EvidenceProposal {
            id: str_field(o, "id"),
            created_at: str_to_i64(&o.get("createdAt").cloned().unwrap_or(Value::Null), "0"),
            updated_at: str_to_i64(&o.get("updatedAt").cloned().unwrap_or(Value::Null), "0"),
            status: str_field_default(o, "status", "pending"),
            title: str_field(o, "title"),
            action: str_field(o, "action"),
            type_name: str_field(o, "typeName"),
            node_id: str_to_u64(&o.get("nodeId").cloned().unwrap_or(Value::Null), "0"),
            field_offset: if o.get("fieldOffset").is_some() {
                o.get("fieldOffset").and_then(Value::as_i64).unwrap_or(-1) as i32
            } else {
                -1
            },
            confidence: o.get("confidence").and_then(Value::as_f64).unwrap_or(0.0),
            evidence_ids: str_array(o, "evidenceIds"),
            operations: array_or_null(o, "operations"),
            data: object_or_null(o, "data"),
        }
    }
}

/// `o[key].toString()` — empty string on miss / non-string.
fn str_field(o: &Value, key: &str) -> String {
    o.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

/// `o[key].toString(default)` — the default on miss / non-string.
fn str_field_default(o: &Value, key: &str, default: &str) -> String {
    o.get(key)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_string()
}

/// `for (v : o[key].toArray()) out.append(v.toString())`.
fn str_array(o: &Value, key: &str) -> Vec<String> {
    match o.get(key).and_then(Value::as_array) {
        Some(arr) => arr
            .iter()
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect(),
        None => Vec::new(),
    }
}

/// `o[key].toObject()` → preserved verbatim, or `Null` when absent/empty.
/// Matches Qt's `QJsonValue::toObject()` (an empty object when absent) but we
/// store `Null` for "no payload" so `toJson` re-omits it exactly like C++.
fn object_or_null(o: &Value, key: &str) -> Value {
    match o.get(key) {
        Some(v) if v.is_object() => v.clone(),
        _ => Value::Null,
    }
}

/// `o[key].toArray()` → preserved verbatim, or `Null` when absent/empty.
fn array_or_null(o: &Value, key: &str) -> Value {
    match o.get(key) {
        Some(v) if v.is_array() => v.clone(),
        _ => Value::Null,
    }
}

/// `!obj.isEmpty()` for a stored `data` payload.
fn json_object_non_empty(v: &Value) -> bool {
    matches!(v, Value::Object(m) if !m.is_empty())
}

/// `!arr.isEmpty()` for a stored `operations` payload.
fn json_array_non_empty(v: &Value) -> bool {
    matches!(v, Value::Array(a) if !a.is_empty())
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

    // ── Evidence records ──

    #[test]
    fn parse_prefixed_sequence_matches_cpp() {
        assert_eq!(parse_prefixed_sequence("ev_42", "ev_"), 42);
        assert_eq!(parse_prefixed_sequence("hyp_7", "hyp_"), 7);
        assert_eq!(parse_prefixed_sequence("ev_42", "hyp_"), 0); // wrong prefix
        assert_eq!(parse_prefixed_sequence("ev_x", "ev_"), 0); // non-numeric
        assert_eq!(parse_prefixed_sequence("", "ev_"), 0);
    }

    #[test]
    fn evidence_event_defaults_omit_optional_keys() {
        // A default-constructed event: id="", timestamp=0, fieldOffset=-1,
        // confidence=-1.0, everything else empty. Only id+timestamp emit.
        let e = EvidenceEvent::default();
        assert_eq!(e.field_offset, -1);
        assert_eq!(e.confidence, -1.0);
        let j = e.to_json();
        let o = j.as_object().unwrap();
        assert_eq!(o.len(), 2, "only id+timestamp should be present: {o:?}");
        assert!(o.contains_key("id"));
        assert_eq!(o["timestamp"], json!("0"));
        // Negative fieldOffset/confidence are omitted (matches C++).
        assert!(!o.contains_key("fieldOffset"));
        assert!(!o.contains_key("confidence"));
        assert!(!o.contains_key("nodeId"));
    }

    #[test]
    fn evidence_event_round_trip_all_fields() {
        let e = EvidenceEvent {
            id: "ev_5".into(),
            timestamp: 1_700_000_000_000,
            source: "ida".into(),
            kind: "field_access".into(),
            summary: "read at Player+0x10".into(),
            type_name: "Player".into(),
            node_id: 99,
            field_offset: 16,
            address: "<game.exe>+0x10".into(),
            function_name: "tick".into(),
            function_address: "0x401000".into(),
            instruction: "mov eax,[rcx+0x10]".into(),
            confidence: 0.75,
            tags: vec!["auto".into(), "ida".into()],
            data: json!({ "k": "v" }),
        };
        let back = EvidenceEvent::from_json(&e.to_json());
        assert_eq!(back, e);
    }

    #[test]
    fn evidence_event_fieldoffset_zero_is_preserved() {
        // fieldOffset==0 must serialize (>=0) and survive the round-trip.
        let e = EvidenceEvent {
            id: "ev_1".into(),
            field_offset: 0,
            ..EvidenceEvent::default()
        };
        let j = e.to_json();
        assert_eq!(j["fieldOffset"], json!(0));
        assert_eq!(EvidenceEvent::from_json(&j).field_offset, 0);
    }

    #[test]
    fn evidence_hypothesis_defaults_and_round_trip() {
        let h = EvidenceHypothesis::default();
        assert_eq!(h.status, "open");
        assert_eq!(h.field_offset, -1);
        let j = h.to_json();
        let o = j.as_object().unwrap();
        // id, createdAt, updatedAt, status, confidence always present.
        assert!(o.contains_key("id"));
        assert!(o.contains_key("createdAt"));
        assert!(o.contains_key("updatedAt"));
        assert_eq!(o["status"], json!("open"));
        assert_eq!(o["confidence"], json!(0.0));
        assert!(!o.contains_key("fieldOffset"));

        let full = EvidenceHypothesis {
            id: "hyp_3".into(),
            created_at: 100,
            updated_at: 200,
            status: "confirmed".into(),
            claim: "health".into(),
            label: "hp".into(),
            type_name: "Player".into(),
            node_id: 7,
            field_offset: 0x230,
            confidence: 0.9,
            supporting_evidence_ids: vec!["ev_1".into(), "ev_2".into()],
            contradicting_evidence_ids: vec!["ev_9".into()],
            recommended_validation: vec!["watch".into()],
            notes: "n".into(),
            data: json!({ "x": 1 }),
        };
        assert_eq!(EvidenceHypothesis::from_json(&full.to_json()), full);
    }

    #[test]
    fn evidence_proposal_defaults_and_round_trip() {
        let p = EvidenceProposal::default();
        assert_eq!(p.status, "pending");
        let j = p.to_json();
        let o = j.as_object().unwrap();
        assert_eq!(o["status"], json!("pending"));
        assert_eq!(o["confidence"], json!(0.0));
        assert!(!o.contains_key("operations"));
        assert!(!o.contains_key("data"));

        let full = EvidenceProposal {
            id: "prop_2".into(),
            created_at: 1,
            updated_at: 2,
            status: "applied".into(),
            title: "rename".into(),
            action: "rename_field".into(),
            type_name: "Player".into(),
            node_id: 12,
            field_offset: 4,
            confidence: 0.5,
            evidence_ids: vec!["ev_1".into()],
            operations: json!([{ "op": "rename", "to": "hp" }]),
            data: json!({ "src": "llm" }),
        };
        assert_eq!(EvidenceProposal::from_json(&full.to_json()), full);
    }
}
