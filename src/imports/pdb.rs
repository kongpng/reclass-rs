//! PDB type & symbol import via the `pdb2` crate.
//!
//! Faithful 1:1 port of `src/imports/import_pdb.cpp` (1411 lines). The C++ was
//! entirely `#ifdef _WIN32` (RawPDB + Win32 mmap); `pdb2` is pure Rust and
//! portable, so the public API is cross-platform (ARCHITECTURE.md §3,
//! crate_selection.md). `pdb2` decodes the variable-length CodeView numeric
//! leaves internally, so the C++ `leafSize`/`leafName`/`leafValue` helpers are
//! gone. See BMAP §5.

use std::collections::HashMap;
use std::path::Path;

use pdb2::{
    AddressMap, FallibleIterator, SymbolData, TypeData, TypeFinder, TypeIndex, TypeInformation,
    Variant, PDB,
};

use crate::core::kind::{size_for_kind, NodeKind};
use crate::core::node::{BitfieldMember, Node};
use crate::core::tree::NodeTree;

use super::ImportError;

/// `struct PdbSymbol` (`import_pdb.h:11-15`).
#[derive(Clone, Debug, Default)]
pub struct PdbSymbol {
    pub name: String,
    pub rva: u32,
    /// TPI type index (0 = unknown / public symbol).
    pub type_index: u32,
}

/// `struct PdbSymbolResult` (`import_pdb.h:17-20`).
#[derive(Clone, Debug, Default)]
pub struct PdbSymbolResult {
    pub module_name: String,
    pub symbols: Vec<PdbSymbol>,
}

/// `struct PdbTypeInfo` (`import_pdb.h`).
#[derive(Clone, Debug, Default)]
pub struct PdbTypeInfo {
    pub type_index: u32,
    pub name: String,
    pub size: u64,
    pub child_count: i32,
    pub is_union: bool,
    pub is_enum: bool,
}

/// Progress callback type. `false` = cancel (cpp `ProgressCb`).
pub type ProgressCb<'a> = dyn FnMut(i32, i32) -> bool + 'a;

// ── Primitive mapping (cpp:147-222) ──

/// `mapPrimitiveType(typeIndex)` (`import_pdb.cpp:147-212`) — raw-bit approach,
/// VERBATIM. For type indices `< 0x1000`, mask `& 0xFF` for the base type.
fn map_primitive(type_index: u32) -> NodeKind {
    let base = type_index & 0xFF;
    match base {
        0x03 => NodeKind::Hex8,   // void
        0x10 => NodeKind::Int8,   // signed char
        0x20 => NodeKind::UInt8,  // unsigned char
        0x70 => NodeKind::Int8,   // real char
        0x71 => NodeKind::UInt16, // wchar
        0x7c => NodeKind::UInt8,  // char8
        0x7a => NodeKind::UInt16, // char16
        0x7b => NodeKind::UInt32, // char32
        0x11 => NodeKind::Int16,  // short
        0x21 => NodeKind::UInt16, // ushort
        0x12 => NodeKind::Int32,  // long
        0x22 => NodeKind::UInt32, // ulong
        0x68 => NodeKind::Int8,   // int8
        0x69 => NodeKind::UInt8,  // uint8
        0x72 => NodeKind::Int16,  // int16
        0x73 => NodeKind::UInt16, // uint16
        0x74 => NodeKind::Int32,  // int32
        0x75 => NodeKind::UInt32, // uint32
        0x13 => NodeKind::Int64,  // quad (int64)
        0x23 => NodeKind::UInt64, // uquad (uint64)
        0x76 => NodeKind::Int64,  // int64
        0x77 => NodeKind::UInt64, // uint64
        0x40 => NodeKind::Float,
        0x41 => NodeKind::Double,
        0x30 => NodeKind::Bool,
        0x31 => NodeKind::UInt16, // bool16
        0x32 => NodeKind::UInt32, // bool32
        0x33 => NodeKind::UInt64, // bool64
        0x08 => NodeKind::UInt32, // HRESULT
        0x60 => NodeKind::UInt8,  // bit
        0x78 => NodeKind::Hex64,  // int128 best-effort
        0x79 => NodeKind::Hex64,  // uint128
        _ => NodeKind::Hex32,
    }
}

/// `hexForSize(len)` (`import_pdb.cpp:214-222`).
fn hex_for_size(len: u64) -> NodeKind {
    match len {
        1 => NodeKind::Hex8,
        2 => NodeKind::Hex16,
        4 => NodeKind::Hex32,
        8 => NodeKind::Hex64,
        _ => NodeKind::Hex32,
    }
}

// ── Type table wrapper over pdb2's TypeFinder ──
//
// In C++, `TypeTable` exposed `firstIndex()`/`lastIndex()` (one-past-last) and
// O(1) `get(idx)`. `pdb2`'s `TypeFinder` gives `find(idx)` (parse-on-demand)
// and the `TypeInformation` knows the min/max indices. C++ `firstIndex` ==
// pdb2 `minimum_index`; C++ `lastIndex` (one-past-last) == `maximum_index + 1`.

struct TypeTable<'t> {
    finder: TypeFinder<'t>,
    first_index: u32,
    /// one-past-last (C++ semantics).
    last_index: u32,
}

impl<'t> TypeTable<'t> {
    fn build<'s: 't>(info: &'t TypeInformation<'s>) -> Self {
        let mut finder = info.finder();
        let mut iter = info.iter();
        while let Ok(Some(_typ)) = iter.next() {
            finder.update(&iter);
        }
        let first_index = info.iter().next().ok().flatten().map(|t| t.index().0);
        // minimum/maximum from the finder.
        let first = first_index.unwrap_or_else(|| finder_min(&finder));
        let max = finder.max_index().0;
        TypeTable {
            finder,
            first_index: first,
            last_index: max.saturating_add(1),
        }
    }

    fn first_index(&self) -> u32 {
        self.first_index
    }
    fn last_index(&self) -> u32 {
        self.last_index
    }

    /// O(1)-ish parse of a type record; `None` on a primitive index or miss.
    fn get(&self, type_index: u32) -> Option<TypeData<'t>> {
        if type_index < self.first_index || type_index >= self.last_index {
            return None;
        }
        self.finder
            .find(TypeIndex(type_index))
            .ok()
            .and_then(|item| item.parse().ok())
    }
}

/// Derive the finder's minimum index. `TypeFinder` does not expose it directly,
/// but `find(min)` of a primitive returns Primitive; we recover it via the
/// `TypeInformation` iterator's first index in `build`. This fallback computes
/// it from a default of `0x1000` (the standard TPI minimum).
fn finder_min(_finder: &TypeFinder) -> u32 {
    0x1000
}

// ── Import context (cpp:232-256) ──

struct PdbCtx<'t> {
    tree: NodeTree,
    tt: &'t TypeTable<'t>,
    /// typeIndex → nodeId.
    type_cache: HashMap<u32, u64>,
    /// struct/class definition name → typeIndex.
    struct_def_by_name: HashMap<String, u32>,
    /// union definition name → typeIndex.
    union_def_by_name: HashMap<String, u32>,
    udt_def_index_built: bool,
}

/// Whether a name is "anonymous" / compiler-generated (cpp: `name[0] == '<'` /
/// empty).
fn is_anon_name(name: &str) -> bool {
    name.is_empty() || name.starts_with('<')
}

impl<'t> PdbCtx<'t> {
    fn new(tt: &'t TypeTable<'t>) -> Self {
        PdbCtx {
            tree: NodeTree::default(),
            tt,
            type_cache: HashMap::new(),
            struct_def_by_name: HashMap::new(),
            union_def_by_name: HashMap::new(),
            udt_def_index_built: false,
        }
    }

    /// `unwrapModifier` (cpp:248-255).
    fn unwrap_modifier(&self, type_index: u32) -> u32 {
        if type_index < self.tt.first_index() {
            return type_index;
        }
        match self.tt.get(type_index) {
            Some(TypeData::Modifier(m)) => m.underlying_type.0,
            _ => type_index,
        }
    }

    /// `buildUdtDefinitionIndex` (cpp:258-287) — record FIRST typeIndex per
    /// name for non-fwdref LF_UNION/STRUCTURE/CLASS.
    fn build_udt_definition_index(&mut self) {
        if self.udt_def_index_built {
            return;
        }
        self.udt_def_index_built = true;

        for ti in self.tt.first_index()..self.tt.last_index() {
            let rec = match self.tt.get(ti) {
                Some(r) => r,
                None => continue,
            };
            match rec {
                TypeData::Union(u) => {
                    if u.properties.forward_reference() {
                        continue;
                    }
                    let name = u.name.to_string().into_owned();
                    if is_anon_name(&name) {
                        continue;
                    }
                    self.union_def_by_name.entry(name).or_insert(ti);
                }
                TypeData::Class(c) => {
                    if c.properties.forward_reference() {
                        continue;
                    }
                    let name = c.name.to_string().into_owned();
                    if is_anon_name(&name) {
                        continue;
                    }
                    self.struct_def_by_name.entry(name).or_insert(ti);
                }
                _ => {}
            }
        }
    }

    /// `findUdtDefinitionIndex(kind, name)` (cpp:289-306). `is_union` selects
    /// the union vs struct/class lookup.
    fn find_udt_definition_index(&mut self, is_union: bool, name: &str) -> u32 {
        if name.is_empty() {
            return 0;
        }
        self.build_udt_definition_index();
        if is_union {
            self.union_def_by_name.get(name).copied().unwrap_or(0)
        } else {
            self.struct_def_by_name.get(name).copied().unwrap_or(0)
        }
    }

    /// `importUDT(typeIndex)` (cpp:308-358) — returns node id (0 = fail).
    fn import_udt(&mut self, type_index: u32) -> u64 {
        if type_index < self.tt.first_index() {
            return 0;
        }
        if let Some(&id) = self.type_cache.get(&type_index) {
            return id;
        }
        let rec = match self.tt.get(type_index) {
            Some(r) => r,
            None => return 0,
        };

        let (name, field_list_index, is_union) = match rec {
            TypeData::Class(c) => {
                if c.properties.forward_reference() {
                    return 0;
                }
                let fl = c.fields.map(|f| f.0).unwrap_or(0);
                (c.name.to_string().into_owned(), fl, false)
            }
            TypeData::Union(u) => {
                if u.properties.forward_reference() {
                    return 0;
                }
                (u.name.to_string().into_owned(), u.fields.0, true)
            }
            _ => return 0,
        };

        let qname = if name.is_empty() {
            "<anon>".to_string()
        } else {
            name
        };

        let s = Node {
            kind: NodeKind::Struct,
            name: qname.clone(),
            struct_type_name: qname,
            class_keyword: if is_union {
                "union".to_string()
            } else {
                "struct".to_string()
            },
            parent_id: 0,
            collapsed: true,
            ..Node::default()
        };
        let idx = self.tree.add_node(s);
        let node_id = self.tree.nodes[idx].id;

        // Cache the id BEFORE recursing so self/mutual refs resolve.
        self.type_cache.insert(type_index, node_id);

        self.import_field_list(field_list_index, node_id);
        node_id
    }

    /// `importEnum(typeIndex)` (cpp:360-411) — returns node id (0 = fail).
    fn import_enum(&mut self, type_index: u32) -> u64 {
        if type_index < self.tt.first_index() {
            return 0;
        }
        if let Some(&id) = self.type_cache.get(&type_index) {
            return id;
        }
        let rec = match self.tt.get(type_index) {
            Some(r) => r,
            None => return 0,
        };
        let en = match rec {
            TypeData::Enumeration(e) => e,
            _ => return 0,
        };
        if en.properties.forward_reference() {
            return 0;
        }

        let name = en.name.to_string().into_owned();
        let qname = if name.is_empty() {
            "<anon>".to_string()
        } else {
            name
        };

        let mut s = Node {
            kind: NodeKind::Struct,
            name: qname.clone(),
            struct_type_name: qname,
            class_keyword: "enum".to_string(),
            parent_id: 0,
            collapsed: true,
            ..Node::default()
        };

        // Walk its field list, collecting Enumerate members until the first
        // non-Enumerate (cpp:383-405).
        if let Some(TypeData::FieldList(fl)) = self.tt.get(en.fields.0) {
            for field in &fl.fields {
                match field {
                    TypeData::Enumerate(en_member) => {
                        let val = variant_to_i64(&en_member.value);
                        s.enum_members
                            .push((en_member.name.to_string().into_owned(), val));
                    }
                    _ => break,
                }
            }
        }

        let idx = self.tree.add_node(s);
        let node_id = self.tree.nodes[idx].id;
        self.type_cache.insert(type_index, node_id);
        node_id
    }

    /// `importFieldList(idx, parentId)` (cpp:413-555).
    fn import_field_list(&mut self, field_list_index: u32, parent_id: u64) {
        let fl = match self.tt.get(field_list_index) {
            Some(TypeData::FieldList(fl)) => fl,
            _ => return,
        };

        // Bitfield grouping keyed by (offset, slot_size) → container node id.
        let mut bitfield_node_ids: HashMap<(i32, i32), u64> = HashMap::new();

        for field in &fl.fields {
            match field {
                TypeData::Member(m) => {
                    let offset = m.offset as i32;
                    let name = m.name.to_string().into_owned();
                    let member_type = m.field_type.0;

                    // Check for bitfield type (unwrap modifier first).
                    let resolved_type = self.unwrap_modifier(member_type);
                    if let Some(TypeData::Bitfield(bf)) = self.tt.get(resolved_type) {
                        let underlying = bf.underlying_type.0;
                        let bit_len = bf.length;
                        let bit_pos = bf.position;

                        let mut slot_size: i32 = 4;
                        if underlying < self.tt.first_index() {
                            let k = map_primitive(underlying);
                            slot_size = size_for_kind(k);
                        }

                        let key = (offset, slot_size);
                        let bf_node_id = *bitfield_node_ids.entry(key).or_insert_with(|| {
                            let n = Node {
                                kind: NodeKind::Struct,
                                class_keyword: "bitfield".to_string(),
                                element_kind: hex_for_size(slot_size as u64),
                                parent_id,
                                offset,
                                collapsed: false,
                                ..Node::default()
                            };
                            let idx = self.tree.add_node(n);
                            self.tree.nodes[idx].id
                        });
                        let bf_idx = self.tree.index_of_id(bf_node_id);
                        if bf_idx >= 0 {
                            self.tree.nodes[bf_idx as usize].bitfield_members.push(
                                BitfieldMember {
                                    name,
                                    bit_offset: bit_pos,
                                    bit_width: bit_len,
                                },
                            );
                        }
                    } else {
                        self.import_member_type(member_type, offset, name, parent_id);
                    }
                }
                // Continuation of field list in another record (LF_INDEX).
                TypeData::FieldList(_) => {
                    // pdb2 inlines continuations into `fields` already in some
                    // cases, but explicit continuation is via `fl.continuation`.
                }
                // BaseClass / VirtualBaseClass / VFuncTab / Nested / StaticMember
                // / Method / OverloadedMethod / Enumerate → skip.
                _ => {}
            }
        }

        // Continuation (LF_INDEX): recurse into the next field-list record.
        if let Some(cont) = fl.continuation {
            self.import_field_list(cont.0, parent_id);
        }
    }

    /// `importMemberType(idx, offset, name, parentId)` (cpp:557-899) — emit
    /// exactly one node.
    fn import_member_type(&mut self, type_index: u32, offset: i32, name: String, parent_id: u64) {
        // Primitive type indices (< firstIndex) (cpp:559-602)
        if type_index < self.tt.first_index() {
            let ptr_mode = (type_index >> 8) & 0xF;
            if ptr_mode == 0x04 || ptr_mode == 0x05 {
                self.add_leaf(NodeKind::Pointer32, name, parent_id, offset, true);
                return;
            }
            if ptr_mode == 0x06 {
                self.add_leaf(NodeKind::Pointer64, name, parent_id, offset, true);
                return;
            }
            if ptr_mode != 0x00 {
                self.add_leaf(NodeKind::Pointer32, name, parent_id, offset, true);
                return;
            }
            self.add_leaf(map_primitive(type_index), name, parent_id, offset, false);
            return;
        }

        let rec = match self.tt.get(type_index) {
            Some(r) => r,
            None => {
                self.add_leaf(NodeKind::Hex32, name, parent_id, offset, false);
                return;
            }
        };

        match rec {
            TypeData::Modifier(m) => {
                self.import_member_type(m.underlying_type.0, offset, name, parent_id);
            }
            TypeData::Pointer(p) => {
                let ptr_size = p.attributes.size() as u32;
                let pointee = p.underlying_type.0;
                let real_pointee = self.unwrap_modifier(pointee);

                let mut kind = if ptr_size <= 4 {
                    NodeKind::Pointer32
                } else {
                    NodeKind::Pointer64
                };
                let mut ref_id: u64 = 0;

                if real_pointee >= self.tt.first_index() {
                    if let Some(pointee_rec) = self.tt.get(real_pointee) {
                        match pointee_rec {
                            TypeData::Class(_) | TypeData::Union(_) => {
                                let (is_fwd, is_union, pt_name) = match &pointee_rec {
                                    TypeData::Union(u) => (
                                        u.properties.forward_reference(),
                                        true,
                                        u.name.to_string().into_owned(),
                                    ),
                                    TypeData::Class(c) => (
                                        c.properties.forward_reference(),
                                        false,
                                        c.name.to_string().into_owned(),
                                    ),
                                    _ => unreachable!(),
                                };
                                let mut def_index = real_pointee;
                                let mut def_name = pt_name.clone();
                                if is_fwd {
                                    let resolved =
                                        self.find_udt_definition_index(is_union, &pt_name);
                                    if resolved != 0 {
                                        def_index = resolved;
                                        // Refresh the def name from the resolved record.
                                        if let Some(def_rec) = self.tt.get(def_index) {
                                            def_name = match def_rec {
                                                TypeData::Union(u) => {
                                                    u.name.to_string().into_owned()
                                                }
                                                TypeData::Class(c) => {
                                                    c.name.to_string().into_owned()
                                                }
                                                _ => def_name,
                                            };
                                        }
                                    }
                                }
                                if !is_anon_name(&def_name) {
                                    ref_id = self.import_udt(def_index);
                                }
                            }
                            TypeData::Procedure(_) | TypeData::MemberFunction(_) => {
                                kind = if ptr_size <= 4 {
                                    NodeKind::FuncPtr32
                                } else {
                                    NodeKind::FuncPtr64
                                };
                            }
                            _ => {}
                        }
                    }
                }

                let n = Node {
                    kind,
                    name,
                    parent_id,
                    offset,
                    collapsed: true,
                    ref_id,
                    ..Node::default()
                };
                self.tree.add_node(n);
            }
            TypeData::Class(_) | TypeData::Union(_) => {
                let (is_fwd, is_union, type_name) = match &rec {
                    TypeData::Union(u) => (
                        u.properties.forward_reference(),
                        true,
                        u.name.to_string().into_owned(),
                    ),
                    TypeData::Class(c) => (
                        c.properties.forward_reference(),
                        false,
                        c.name.to_string().into_owned(),
                    ),
                    _ => unreachable!(),
                };

                let mut def_index = type_index;
                if is_fwd {
                    let resolved = self.find_udt_definition_index(is_union, &type_name);
                    if resolved != 0 {
                        def_index = resolved;
                    }
                }

                // Anonymous types: inline fields directly (cpp:716-744).
                if is_anon_name(&type_name) {
                    let field_list_idx = match self.tt.get(def_index) {
                        Some(TypeData::Union(u)) => u.fields.0,
                        Some(TypeData::Class(c)) => c.fields.map(|f| f.0).unwrap_or(0),
                        _ => 0,
                    };
                    if field_list_idx != 0 {
                        let n = Node {
                            kind: NodeKind::Struct,
                            name,
                            class_keyword: if is_union {
                                "union".to_string()
                            } else {
                                "struct".to_string()
                            },
                            parent_id,
                            offset,
                            collapsed: true,
                            ..Node::default()
                        };
                        let idx = self.tree.add_node(n);
                        let inline_id = self.tree.nodes[idx].id;
                        self.import_field_list(field_list_idx, inline_id);
                        return;
                    }
                    // Fallthrough if no field list.
                }

                let ref_id = self.import_udt(def_index);
                let n = Node {
                    kind: NodeKind::Struct,
                    name,
                    struct_type_name: type_name,
                    class_keyword: if is_union {
                        "union".to_string()
                    } else {
                        "struct".to_string()
                    },
                    parent_id,
                    offset,
                    ref_id,
                    collapsed: true,
                    ..Node::default()
                };
                self.tree.add_node(n);
            }
            TypeData::Array(arr) => {
                let elem_type = arr.element_type.0;
                // pdb2's `dimensions` is the cumulative byte sizes per dim; the
                // total byte size is the last (highest) dimension.
                let total_size = arr.dimensions.last().copied().unwrap_or(0) as u64;

                let real_elem_type = self.unwrap_modifier(elem_type);
                let elem_size: u64 = if real_elem_type < self.tt.first_index() {
                    let ek = map_primitive(real_elem_type);
                    size_for_kind(ek) as u64
                } else {
                    match self.tt.get(real_elem_type) {
                        Some(TypeData::Class(c)) => c.size,
                        Some(TypeData::Union(u)) => u.size,
                        Some(TypeData::Pointer(p)) => p.attributes.size() as u64,
                        Some(TypeData::Enumeration(e)) => {
                            let ut = e.underlying_type.0;
                            if ut < self.tt.first_index() {
                                size_for_kind(map_primitive(ut)) as u64
                            } else {
                                4
                            }
                        }
                        Some(TypeData::Array(a2)) => {
                            a2.dimensions.last().copied().unwrap_or(0) as u64
                        }
                        _ => 0,
                    }
                };

                let count = if elem_size > 0 {
                    (total_size / elem_size) as i32
                } else {
                    1
                };

                let mut n = Node {
                    kind: NodeKind::Array,
                    name,
                    parent_id,
                    offset,
                    array_len: count,
                    ..Node::default()
                };

                if real_elem_type < self.tt.first_index() {
                    n.element_kind = map_primitive(real_elem_type);
                } else {
                    match self.tt.get(real_elem_type) {
                        Some(TypeData::Class(_)) | Some(TypeData::Union(_)) => {
                            n.element_kind = NodeKind::Struct;
                            n.ref_id = self.import_udt(real_elem_type);
                            let tn = match self.tt.get(real_elem_type) {
                                Some(TypeData::Union(u)) => u.name.to_string().into_owned(),
                                Some(TypeData::Class(c)) => c.name.to_string().into_owned(),
                                _ => String::new(),
                            };
                            if !tn.is_empty() {
                                n.struct_type_name = tn;
                            }
                        }
                        Some(TypeData::Pointer(p)) => {
                            let sz = p.attributes.size();
                            n.element_kind = if sz <= 4 {
                                NodeKind::Pointer32
                            } else {
                                NodeKind::Pointer64
                            };
                        }
                        _ => {
                            n.element_kind = hex_for_size(elem_size);
                        }
                    }
                }
                self.tree.add_node(n);
            }
            TypeData::Enumeration(e) => {
                let utype = e.underlying_type.0;
                let enum_node_id = self.import_enum(type_index);
                let kind = if utype < self.tt.first_index() {
                    map_primitive(utype)
                } else {
                    NodeKind::UInt32
                };
                let n = Node {
                    kind,
                    name,
                    parent_id,
                    offset,
                    ref_id: enum_node_id,
                    ..Node::default()
                };
                self.tree.add_node(n);
            }
            TypeData::Procedure(_) | TypeData::MemberFunction(_) => {
                self.add_leaf(NodeKind::Hex64, name, parent_id, offset, false);
            }
            TypeData::Bitfield(bf) => {
                let underlying = bf.underlying_type.0;
                let mut slot_size: i32 = 4;
                if underlying < self.tt.first_index() {
                    slot_size = size_for_kind(map_primitive(underlying));
                }
                let mut n = Node {
                    kind: NodeKind::Struct,
                    class_keyword: "bitfield".to_string(),
                    element_kind: hex_for_size(slot_size as u64),
                    name: name.clone(),
                    parent_id,
                    offset,
                    ..Node::default()
                };
                n.bitfield_members.push(BitfieldMember {
                    name,
                    bit_offset: bf.position,
                    bit_width: bf.length,
                });
                self.tree.add_node(n);
            }
            _ => {
                // Unknown complex type — emit as Hex32.
                self.add_leaf(NodeKind::Hex32, name, parent_id, offset, false);
            }
        }
    }

    fn add_leaf(
        &mut self,
        kind: NodeKind,
        name: String,
        parent_id: u64,
        offset: i32,
        collapsed: bool,
    ) {
        let n = Node {
            kind,
            name,
            parent_id,
            offset,
            collapsed,
            ..Node::default()
        };
        self.tree.add_node(n);
    }
}

fn variant_to_i64(v: &Variant) -> i64 {
    match *v {
        Variant::U8(x) => x as i64,
        Variant::U16(x) => x as i64,
        Variant::U32(x) => x as i64,
        Variant::U64(x) => x as i64,
        Variant::I8(x) => x as i64,
        Variant::I16(x) => x as i64,
        Variant::I32(x) => x as i64,
        Variant::I64(x) => x,
    }
}

// ── Open + validate (cpp:902-944) ──

fn open_pdb(path: &Path) -> Result<PDB<'static, std::fs::File>, ImportError> {
    if !path.exists() {
        return Err(ImportError::PdbNotFound);
    }
    let file = std::fs::File::open(path).map_err(|_| ImportError::PdbMapFailed)?;
    let pdb = PDB::open(file).map_err(|_| ImportError::PdbInvalid)?;
    Ok(pdb)
}

// ── Public API ──

/// `extractPdbSymbols` (cpp:948-1061).
pub fn extract_pdb_symbols(path: &Path) -> Result<PdbSymbolResult, ImportError> {
    let mut pdb = open_pdb(path)?;

    let address_map: AddressMap = pdb.address_map().map_err(|_| ImportError::PdbNoDbi)?;

    let mut result = PdbSymbolResult::default();
    result.module_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    let symbol_table = pdb.global_symbols().map_err(|_| ImportError::PdbNoDbi)?;
    let mut iter = symbol_table.iter();
    while let Ok(Some(sym)) = iter.next() {
        let data = match sym.parse() {
            Ok(d) => d,
            Err(_) => continue,
        };
        match data {
            SymbolData::Public(p) => {
                let rva = match p.offset.to_rva(&address_map) {
                    Some(r) => r.0,
                    None => continue,
                };
                if rva == 0 {
                    continue;
                }
                result.symbols.push(PdbSymbol {
                    name: p.name.to_string().into_owned(),
                    rva,
                    type_index: 0,
                });
            }
            SymbolData::Data(d) => {
                let rva = match d.offset.to_rva(&address_map) {
                    Some(r) => r.0,
                    None => continue,
                };
                if rva == 0 {
                    continue;
                }
                let name = d.name.to_string().into_owned();
                if name.is_empty() {
                    continue;
                }
                result.symbols.push(PdbSymbol {
                    name,
                    rva,
                    type_index: d.type_index.0,
                });
            }
            SymbolData::ThreadStorage(t) => {
                let rva = match t.offset.to_rva(&address_map) {
                    Some(r) => r.0,
                    None => continue,
                };
                if rva == 0 {
                    continue;
                }
                let name = t.name.to_string().into_owned();
                if name.is_empty() {
                    continue;
                }
                result.symbols.push(PdbSymbol {
                    name,
                    rva,
                    type_index: t.type_index.0,
                });
            }
            _ => {}
        }
    }

    Ok(result)
}

/// `enumeratePdbTypes` (cpp:1065-1160).
pub fn enumerate_pdb_types(path: &Path) -> Result<Vec<PdbTypeInfo>, ImportError> {
    let mut pdb = open_pdb(path)?;
    let info = pdb.type_information().map_err(|_| ImportError::PdbNoTpi)?;
    let tt = TypeTable::build(&info);

    let mut result: Vec<PdbTypeInfo> = Vec::new();
    for ti in tt.first_index()..tt.last_index() {
        let rec = match tt.get(ti) {
            Some(r) => r,
            None => continue,
        };
        let (name, field_count, is_union, is_enum, size) = match rec {
            TypeData::Enumeration(e) => {
                if e.properties.forward_reference() {
                    continue;
                }
                let ut = e.underlying_type.0;
                let size = if ut < tt.first_index() {
                    size_for_kind(map_primitive(ut)) as u64
                } else {
                    4
                };
                (
                    e.name.to_string().into_owned(),
                    e.count as i32,
                    false,
                    true,
                    size,
                )
            }
            TypeData::Union(u) => {
                if u.properties.forward_reference() {
                    continue;
                }
                (
                    u.name.to_string().into_owned(),
                    u.count as i32,
                    true,
                    false,
                    u.size,
                )
            }
            TypeData::Class(c) => {
                if c.properties.forward_reference() {
                    continue;
                }
                (
                    c.name.to_string().into_owned(),
                    c.count as i32,
                    false,
                    false,
                    c.size,
                )
            }
            _ => continue,
        };

        if is_anon_name(&name) {
            continue;
        }

        result.push(PdbTypeInfo {
            type_index: ti,
            name,
            size,
            child_count: field_count,
            is_union,
            is_enum,
        });
    }

    Ok(result)
}

/// `importPdbSelected` (cpp:1164-1207). Returns the partial tree on cancel
/// (matches the C++).
pub fn import_pdb_selected(
    path: &Path,
    type_indices: &[u32],
    mut progress: Option<&mut ProgressCb>,
) -> Result<NodeTree, ImportError> {
    let mut pdb = open_pdb(path)?;
    let info = pdb.type_information().map_err(|_| ImportError::PdbNoTpi)?;
    let tt = TypeTable::build(&info);

    let mut ctx = PdbCtx::new(&tt);
    let total = type_indices.len() as i32;
    for (i, &ti) in type_indices.iter().enumerate() {
        match tt.get(ti) {
            Some(TypeData::Enumeration(_)) => {
                ctx.import_enum(ti);
            }
            _ => {
                ctx.import_udt(ti);
            }
        }
        if let Some(cb) = progress.as_deref_mut() {
            if !cb(i as i32 + 1, total) {
                // Return the partial tree (cpp returns it with "Import cancelled").
                return Ok(ctx.tree);
            }
        }
    }

    if ctx.tree.nodes.is_empty() {
        return Err(ImportError::PdbNoTypesImported);
    }
    Ok(ctx.tree)
}

/// `importPdb` (legacy, cpp:1211-1262). `struct_filter == ""` imports all.
pub fn import_pdb(path: &Path, struct_filter: &str) -> Result<NodeTree, ImportError> {
    let mut pdb = open_pdb(path)?;
    let info = pdb.type_information().map_err(|_| ImportError::PdbNoTpi)?;
    let tt = TypeTable::build(&info);

    let mut ctx = PdbCtx::new(&tt);

    for ti in tt.first_index()..tt.last_index() {
        let rec = match tt.get(ti) {
            Some(r) => r,
            None => continue,
        };
        let (fwdref, name) = match rec {
            TypeData::Union(u) => (
                u.properties.forward_reference(),
                u.name.to_string().into_owned(),
            ),
            TypeData::Class(c) => (
                c.properties.forward_reference(),
                c.name.to_string().into_owned(),
            ),
            _ => continue,
        };
        if fwdref {
            continue;
        }
        // C++: `if (!name) continue;` — empty leaf-name pointer is falsy. An
        // empty string here would be anonymous; the C++ only skips a NULL name,
        // but practically names are non-empty for real UDTs.
        if name.is_empty() {
            continue;
        }
        if !struct_filter.is_empty() && name != struct_filter {
            continue;
        }

        ctx.import_udt(ti);

        if !struct_filter.is_empty() {
            break;
        }
    }

    if ctx.tree.nodes.is_empty() {
        if !struct_filter.is_empty() {
            return Err(ImportError::PdbTypeNotFound(struct_filter.to_string()));
        } else {
            return Err(ImportError::PdbNoTypes);
        }
    }

    Ok(ctx.tree)
}

/// `importTypeForSymbol` (cpp:1266-1375).
pub fn import_type_for_symbol(
    path: &Path,
    type_index: u32,
    type_name_out: &mut String,
) -> Result<NodeTree, ImportError> {
    if type_index == 0 {
        return Err(ImportError::SymbolNoType);
    }

    let mut pdb = open_pdb(path)?;
    let info = pdb.type_information().map_err(|_| ImportError::PdbNoTpi)?;
    let tt = TypeTable::build(&info);

    // Walk Modifier/Pointer chains to the underlying (≤16 deep).
    let mut ti = type_index;
    let mut depth = 0;
    while ti >= tt.first_index() && depth < 16 {
        match tt.get(ti) {
            Some(TypeData::Modifier(m)) => {
                ti = m.underlying_type.0;
                depth += 1;
            }
            Some(TypeData::Pointer(p)) => {
                ti = p.underlying_type.0;
                depth += 1;
            }
            _ => break,
        }
    }

    if ti < tt.first_index() {
        return Err(ImportError::ResolvesToPrimitive(type_index));
    }

    let rec = match tt.get(ti) {
        Some(r) => r,
        None => return Err(ImportError::FailedImportType(ti)),
    };

    let (is_udt, is_enum, is_union, name, fwdref) = match &rec {
        TypeData::Union(u) => (
            true,
            false,
            true,
            u.name.to_string().into_owned(),
            u.properties.forward_reference(),
        ),
        TypeData::Class(c) => (
            true,
            false,
            false,
            c.name.to_string().into_owned(),
            c.properties.forward_reference(),
        ),
        TypeData::Enumeration(e) => (false, true, false, e.name.to_string().into_owned(), false),
        _ => return Err(ImportError::FailedImportType(ti)),
    };

    if !is_udt && !is_enum {
        return Err(ImportError::FailedImportType(ti));
    }

    *type_name_out = name.clone();

    let mut ctx = PdbCtx::new(&tt);

    if is_udt {
        let mut target = ti;
        if fwdref {
            let resolved = ctx.find_udt_definition_index(is_union, &name);
            if resolved != 0 {
                target = resolved;
            }
        }
        ctx.import_udt(target);
    } else {
        ctx.import_enum(ti);
    }

    if ctx.tree.nodes.is_empty() {
        return Err(ImportError::FailedImportType(ti));
    }

    Ok(ctx.tree)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_returns_error() {
        // The ONLY PDB slot runnable cross-platform without a fixture.
        let err = import_pdb(Path::new("nonexistent-xyzzy.pdb"), "");
        assert!(err.is_err());
        match err {
            Err(ImportError::PdbNotFound) => {}
            other => panic!("expected PdbNotFound, got {:?}", other),
        }
    }

    #[test]
    fn map_primitive_known_values() {
        assert_eq!(map_primitive(0x74), NodeKind::Int32);
        assert_eq!(map_primitive(0x23), NodeKind::UInt64);
        assert_eq!(map_primitive(0x03), NodeKind::Hex8);
        assert_eq!(map_primitive(0x40), NodeKind::Float);
        assert_eq!(map_primitive(0x1234), NodeKind::Hex32); // default
    }

    #[test]
    fn hex_for_size_table() {
        assert_eq!(hex_for_size(1), NodeKind::Hex8);
        assert_eq!(hex_for_size(2), NodeKind::Hex16);
        assert_eq!(hex_for_size(4), NodeKind::Hex32);
        assert_eq!(hex_for_size(8), NodeKind::Hex64);
        assert_eq!(hex_for_size(3), NodeKind::Hex32);
    }
}
