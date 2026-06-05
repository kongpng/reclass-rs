//! Source-code emission from the node tree (C/C++, Rust, #define offsets, C#,
//! Python ctypes).
//!
//! Faithful 1:1 port of `src/generator.{h,cpp}`. Every backend reproduces the
//! C++ output byte-for-byte (column alignment via the `\u{1}` marker pass, hex
//! casing, `_pad{:04x}` numbering shared across a whole render, etc.).

use std::collections::{HashMap, HashSet};

use crate::core::{is_hex_node, is_pointer_kind, BitfieldMember, Node, NodeKind, NodeTree};

/// `enum class CodeFormat : int` (`generator.h:11-18`).
#[repr(i32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CodeFormat {
    CppHeader = 0,
    RustStruct,
    DefineOffsets,
    CSharpStruct,
    PythonCtypes,
}

impl CodeFormat {
    /// Decode a persisted index (the C++ `codeFormat` QSettings int → `fmtCombo`
    /// current index; main.cpp:2415/5454). Out-of-range values fall back to the
    /// default ([`CodeFormat::CppHeader`], index 0), matching `QComboBox`'s
    /// clamp-to-valid behaviour for `setCurrentIndex`.
    pub fn from_index(idx: i32) -> CodeFormat {
        match idx {
            0 => CodeFormat::CppHeader,
            1 => CodeFormat::RustStruct,
            2 => CodeFormat::DefineOffsets,
            3 => CodeFormat::CSharpStruct,
            4 => CodeFormat::PythonCtypes,
            _ => CodeFormat::CppHeader,
        }
    }
}

/// `enum class CodeScope : int` (`generator.h:20-25`).
#[repr(i32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CodeScope {
    /// just the selected struct.
    Current = 0,
    /// selected struct + all referenced types.
    WithChildren,
    /// all root-level structs.
    FullSdk,
}

impl CodeScope {
    /// Decode a persisted index (the C++ `codeScope` QSettings int → `scopeCombo`
    /// current index; main.cpp:2441/5456). Out-of-range values fall back to the
    /// default ([`CodeScope::Current`], index 0).
    pub fn from_index(idx: i32) -> CodeScope {
        match idx {
            0 => CodeScope::Current,
            1 => CodeScope::WithChildren,
            2 => CodeScope::FullSdk,
            _ => CodeScope::Current,
        }
    }
}

/// Per-`NodeKind` display-name overrides passed to the renderers.
pub type TypeAliases = HashMap<NodeKind, String>;

// ── Identifier sanitisation (`generator.cpp:13-24`) ──

/// A field's emitted identifier: its sanitized name, or a synthetic
/// `field_<offset:02x>` when unnamed. The C/Rust/C# backends all derive field
/// names this way.
fn default_field_name(node: &Node) -> String {
    sanitize_ident(&if node.name.is_empty() {
        format!("field_{:02x}", node.offset)
    } else {
        node.name.clone()
    })
}

/// `sanitizeIdent(const QString&)` (`generator.cpp:13-24`).
///
/// Unicode-aware (`char::is_alphanumeric`/`is_alphabetic`, mirroring
/// `QChar::isLetterOrNumber`/`isLetter`).
fn sanitize_ident(name: &str) -> String {
    if name.is_empty() {
        return "unnamed".to_string();
    }
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    // `name` is non-empty ⇒ `out` is non-empty (one output char per input char).
    let first = out.chars().next().unwrap();
    if !first.is_alphabetic() && first != '_' {
        out.insert(0, '_');
    }
    out
}

/// `cTypeName(NodeKind)` (`generator.cpp:28-61`).
fn c_type_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Hex8 => "uint8_t",
        NodeKind::Hex16 => "uint16_t",
        NodeKind::Hex32 => "uint32_t",
        NodeKind::Hex64 => "uint64_t",
        NodeKind::Hex128 => "uint8_t", // no C int128; emitted as array in emit_field
        NodeKind::Int8 => "int8_t",
        NodeKind::Int16 => "int16_t",
        NodeKind::Int32 => "int32_t",
        NodeKind::Int64 => "int64_t",
        NodeKind::Int128 => "__int128",
        NodeKind::UInt8 => "uint8_t",
        NodeKind::UInt16 => "uint16_t",
        NodeKind::UInt32 => "uint32_t",
        NodeKind::UInt64 => "uint64_t",
        NodeKind::UInt128 => "unsigned __int128",
        NodeKind::Float16 => "_Float16",
        NodeKind::Float => "float",
        NodeKind::Double => "double",
        NodeKind::Bool => "bool",
        NodeKind::Pointer32 => "uint32_t",
        NodeKind::Pointer64 => "uint64_t",
        NodeKind::FuncPtr32 => "uint32_t",
        NodeKind::FuncPtr64 => "uint64_t",
        NodeKind::Vec2 => "float",
        NodeKind::Vec3 => "float",
        NodeKind::Vec4 => "float",
        NodeKind::Mat4x4 => "float",
        NodeKind::UTF8 => "char",
        NodeKind::UTF16 => "wchar_t",
        _ => "uint8_t",
    }
}

/// `rustTypeName(NodeKind)` (`generator.cpp:507-540`).
fn rust_type_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Hex8 => "u8",
        NodeKind::Hex16 => "u16",
        NodeKind::Hex32 => "u32",
        NodeKind::Hex64 => "u64",
        NodeKind::Hex128 => "u128",
        NodeKind::Int8 => "i8",
        NodeKind::Int16 => "i16",
        NodeKind::Int32 => "i32",
        NodeKind::Int64 => "i64",
        NodeKind::Int128 => "i128",
        NodeKind::UInt8 => "u8",
        NodeKind::UInt16 => "u16",
        NodeKind::UInt32 => "u32",
        NodeKind::UInt64 => "u64",
        NodeKind::UInt128 => "u128",
        NodeKind::Float16 => "f16",
        NodeKind::Float => "f32",
        NodeKind::Double => "f64",
        NodeKind::Bool => "bool",
        NodeKind::Pointer32 => "u32",
        NodeKind::Pointer64 => "u64",
        NodeKind::FuncPtr32 => "u32",
        NodeKind::FuncPtr64 => "u64",
        NodeKind::Vec2 => "f32",
        NodeKind::Vec3 => "f32",
        NodeKind::Vec4 => "f32",
        NodeKind::Mat4x4 => "f32",
        NodeKind::UTF8 => "u8",
        NodeKind::UTF16 => "u16",
        _ => "u8",
    }
}

/// `csTypeName(NodeKind)` (`generator.cpp:852-885`).
fn cs_type_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Hex8 => "byte",
        NodeKind::Hex16 => "ushort",
        NodeKind::Hex32 => "uint",
        NodeKind::Hex64 => "ulong",
        NodeKind::Hex128 => "byte", // emitted as fixed byte[16]
        NodeKind::Int8 => "sbyte",
        NodeKind::Int16 => "short",
        NodeKind::Int32 => "int",
        NodeKind::Int64 => "long",
        NodeKind::Int128 => "Int128",
        NodeKind::UInt8 => "byte",
        NodeKind::UInt16 => "ushort",
        NodeKind::UInt32 => "uint",
        NodeKind::UInt64 => "ulong",
        NodeKind::UInt128 => "UInt128",
        NodeKind::Float16 => "Half",
        NodeKind::Float => "float",
        NodeKind::Double => "double",
        NodeKind::Bool => "bool",
        NodeKind::Pointer32 => "uint",
        NodeKind::Pointer64 => "ulong",
        NodeKind::FuncPtr32 => "uint",
        NodeKind::FuncPtr64 => "ulong",
        NodeKind::Vec2 => "float",
        NodeKind::Vec3 => "float",
        NodeKind::Vec4 => "float",
        NodeKind::Mat4x4 => "float",
        NodeKind::UTF8 => "byte",
        NodeKind::UTF16 => "char",
        _ => "byte",
    }
}

/// `pyTypeName(NodeKind)` (`generator.cpp:1091-1124`).
fn py_type_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Hex8 => "ctypes.c_uint8",
        NodeKind::Hex16 => "ctypes.c_uint16",
        NodeKind::Hex32 => "ctypes.c_uint32",
        NodeKind::Hex64 => "ctypes.c_uint64",
        NodeKind::Hex128 => "ctypes.c_uint8 * 16",
        NodeKind::Int8 => "ctypes.c_int8",
        NodeKind::Int16 => "ctypes.c_int16",
        NodeKind::Int32 => "ctypes.c_int32",
        NodeKind::Int64 => "ctypes.c_int64",
        NodeKind::Int128 => "ctypes.c_int8 * 16",
        NodeKind::UInt8 => "ctypes.c_uint8",
        NodeKind::UInt16 => "ctypes.c_uint16",
        NodeKind::UInt32 => "ctypes.c_uint32",
        NodeKind::UInt64 => "ctypes.c_uint64",
        NodeKind::UInt128 => "ctypes.c_uint8 * 16",
        NodeKind::Float16 => "ctypes.c_uint16", // no native half
        NodeKind::Float => "ctypes.c_float",
        NodeKind::Double => "ctypes.c_double",
        NodeKind::Bool => "ctypes.c_bool",
        NodeKind::Pointer32 => "ctypes.c_uint32",
        NodeKind::Pointer64 => "ctypes.c_uint64",
        NodeKind::FuncPtr32 => "ctypes.c_uint32",
        NodeKind::FuncPtr64 => "ctypes.c_uint64",
        NodeKind::Vec2 => "ctypes.c_float",
        NodeKind::Vec3 => "ctypes.c_float",
        NodeKind::Vec4 => "ctypes.c_float",
        NodeKind::Mat4x4 => "ctypes.c_float",
        NodeKind::UTF8 => "ctypes.c_char",
        NodeKind::UTF16 => "ctypes.c_wchar",
        _ => "ctypes.c_uint8",
    }
}

// ── Offset comment marker + helpers (`generator.cpp:164-174`) ──

/// `static const QChar kCommentMarker = QChar(0x01)` (`generator.cpp:164`).
const COMMENT_MARKER: char = '\u{1}';

/// `offsetComment(int, bool=false)` (`generator.cpp:166-170`). Uppercase hex.
fn offset_comment(offset: i32, is_sizeof: bool) -> String {
    if is_sizeof {
        format!("{}// sizeof 0x{:X}", COMMENT_MARKER, offset)
    } else {
        format!("{}// 0x{:X}", COMMENT_MARKER, offset)
    }
}

/// `indent(int)` (`generator.cpp:172-174`).
fn indent(depth: i32) -> String {
    " ".repeat((depth * 4) as usize)
}

/// `buildChildMap(const NodeTree&)` (`generator.cpp:461-466`). parentId → node
/// indices in insertion order (root under key `0`).
fn build_child_map(tree: &NodeTree) -> HashMap<u64, Vec<usize>> {
    let mut map: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, n) in tree.nodes.iter().enumerate() {
        map.entry(n.parent_id).or_default().push(i);
    }
    map
}

/// `alignComments(const QString&)` (`generator.cpp:472-501`). Two-pass column
/// alignment: replaces the `\u{1}` marker with padding so all offset comments
/// line up to the longest code prefix + 1 space.
fn align_comments(raw: &str) -> String {
    let lines: Vec<&str> = raw.split('\n').collect();

    // Pass 1: maximum code width (byte index of the marker) over all lines.
    let mut max_code = 0usize;
    for line in &lines {
        if let Some(pos) = line.find(COMMENT_MARKER) {
            if pos > max_code {
                max_code = pos;
            }
        }
    }

    // Pass 2: replace markers with padding.
    let mut result = String::with_capacity(raw.len() + lines.len() * 8);
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            result.push('\n');
        }
        match line.find(COMMENT_MARKER) {
            Some(pos) => {
                result.push_str(&line[..pos]);
                let pad = (max_code - pos + 1).max(1);
                result.push_str(&" ".repeat(pad));
                result.push_str(&line[pos + COMMENT_MARKER.len_utf8()..]);
            }
            None => result.push_str(line),
        }
    }
    result
}

// ── Generator context (`generator.cpp:65-155`) ──

/// `struct GenContext` (`generator.cpp:65-155`) — shared mutable render state.
struct GenContext<'a> {
    tree: &'a NodeTree,
    child_map: HashMap<u64, Vec<usize>>,
    emitted_type_names: HashSet<String>,
    emitted_ids: HashSet<u64>,
    visiting: HashSet<u64>,
    forward_declared: HashSet<u64>,
    output: String,
    pad_counter: i32,
    type_aliases: Option<&'a TypeAliases>,
    emit_asserts: bool,
    name_by_id: HashMap<u64, String>,
}

impl<'a> GenContext<'a> {
    fn new(tree: &'a NodeTree, aliases: Option<&'a TypeAliases>, emit_asserts: bool) -> Self {
        GenContext {
            tree,
            child_map: build_child_map(tree),
            emitted_type_names: HashSet::new(),
            emitted_ids: HashSet::new(),
            visiting: HashSet::new(),
            forward_declared: HashSet::new(),
            output: String::new(),
            pad_counter: 0,
            type_aliases: aliases,
            emit_asserts,
            name_by_id: HashMap::new(),
        }
    }

    /// `GenContext::prepareChildren(structId)` (`generator.cpp:87-97`).
    /// Returns `(regular sorted by offset, static fields)`.
    fn prepare_children(&self, struct_id: u64) -> (Vec<usize>, Vec<usize>) {
        let mut children: Vec<usize> = Vec::new();
        let mut static_idxs: Vec<usize> = Vec::new();
        if let Some(kids) = self.child_map.get(&struct_id) {
            for &ci in kids {
                if self.tree.nodes[ci].is_static {
                    static_idxs.push(ci);
                } else {
                    children.push(ci);
                }
            }
        }
        children.sort_by_key(|&i| self.tree.nodes[i].offset);
        (children, static_idxs)
    }

    /// `GenContext::uniquePadName()` (`generator.cpp:99-101`).
    fn unique_pad_name(&mut self) -> String {
        let n = self.pad_counter;
        self.pad_counter += 1;
        format!("_pad{:04x}", n)
    }

    /// A user type-alias for `kind` if one is set (non-empty), else `fallback`.
    /// The shared body of the per-language `c_type` / `rust_type` / `cs_type`.
    fn aliased(&self, kind: NodeKind, fallback: fn(NodeKind) -> &'static str) -> String {
        if let Some(m) = self.type_aliases {
            if let Some(v) = m.get(&kind) {
                if !v.is_empty() {
                    return v.clone();
                }
            }
        }
        fallback(kind).to_string()
    }

    /// `GenContext::cType(NodeKind)` (`generator.cpp:104-111`).
    fn c_type(&self, kind: NodeKind) -> String {
        self.aliased(kind, c_type_name)
    }

    /// `rustType(GenContext&, NodeKind)` (`generator.cpp:545-552`).
    fn rust_type(&self, kind: NodeKind) -> String {
        self.aliased(kind, rust_type_name)
    }

    /// `csType(GenContext&, NodeKind)` (`generator.cpp:890-897`).
    fn cs_type(&self, kind: NodeKind) -> String {
        self.aliased(kind, cs_type_name)
    }

    /// The element struct's emitted name for an Array node `array_id`: the name of
    /// its first Struct child, or None when the element type is a primitive.
    fn array_elem_struct_name(&self, array_id: u64) -> Option<String> {
        for &ak in self.child_map.get(&array_id)? {
            if self.tree.nodes[ak].kind == NodeKind::Struct {
                return Some(self.name_for(&self.tree.nodes[ak]));
            }
        }
        None
    }

    /// `GenContext::structName(const Node&)` (`generator.cpp:114-118`).
    fn struct_name(&self, n: &Node) -> String {
        if !n.struct_type_name.is_empty() {
            sanitize_ident(&n.struct_type_name)
        } else if !n.name.is_empty() {
            sanitize_ident(&n.name)
        } else {
            format!("anon_{:x}", n.id)
        }
    }

    /// `GenContext::nameFor(const Node&)` (`generator.cpp:123-127`).
    fn name_for(&self, n: &Node) -> String {
        match self.name_by_id.get(&n.id) {
            Some(name) => name.clone(),
            None => self.struct_name(n),
        }
    }

    /// `GenContext::assignUniqueNames()` (`generator.cpp:135-154`).
    fn assign_unique_names(&mut self) {
        let mut used: HashSet<String> = HashSet::new();

        // Pass 1: roots (parentId==0 && kind==Struct), in insertion order.
        for i in 0..self.tree.nodes.len() {
            let n = &self.tree.nodes[i];
            if n.parent_id != 0 || n.kind != NodeKind::Struct {
                continue;
            }
            let base = self.struct_name(n);
            let id = n.id;
            let name = uniquify(&mut used, &base);
            self.name_by_id.insert(id, name);
        }
        // Pass 2: nested named structs.
        for i in 0..self.tree.nodes.len() {
            let n = &self.tree.nodes[i];
            if n.parent_id == 0 || n.kind != NodeKind::Struct {
                continue;
            }
            if n.struct_type_name.is_empty() {
                continue;
            }
            let base = self.struct_name(n);
            let id = n.id;
            let name = uniquify(&mut used, &base);
            self.name_by_id.insert(id, name);
        }
    }
}

/// The inner `assign` lambda from `assignUniqueNames` (`generator.cpp:137-144`).
fn uniquify(used: &mut HashSet<String>, base: &str) -> String {
    let mut name = base.to_string();
    let mut suffix = 2;
    while used.contains(&name) {
        name = format!("{}_v{}", base, suffix);
        suffix += 1;
    }
    used.insert(name.clone());
    name
}

/// `(kind==Pointer32 && psize<=4) || (kind==Pointer64 && psize>=8)`.
fn is_native_ptr(kind: NodeKind, pointer_size: i32) -> bool {
    (kind == NodeKind::Pointer32 && pointer_size <= 4)
        || (kind == NodeKind::Pointer64 && pointer_size >= 8)
}

// ═══════════════════════════════════════════════════════════════════
// ── C/C++ backend ──
// ═══════════════════════════════════════════════════════════════════

/// `emitField(GenContext&, const Node&, int, int)` (`generator.cpp:176-221`).
fn emit_field(ctx: &GenContext, node: &Node, depth: i32, base_offset: i32) -> String {
    let tree = ctx.tree;
    let ind = indent(depth);
    let name = default_field_name(node);
    let oc = offset_comment(base_offset + node.offset, false);

    match node.kind {
        NodeKind::Vec2 => format!("{}{} {}[2];{}", ind, ctx.c_type(NodeKind::Float), name, oc),
        NodeKind::Vec3 => format!("{}{} {}[3];{}", ind, ctx.c_type(NodeKind::Float), name, oc),
        NodeKind::Vec4 => format!("{}{} {}[4];{}", ind, ctx.c_type(NodeKind::Float), name, oc),
        NodeKind::Mat4x4 => {
            format!(
                "{}{} {}[4][4];{}",
                ind,
                ctx.c_type(NodeKind::Float),
                name,
                oc
            )
        }
        NodeKind::UTF8 => format!(
            "{}{} {}[{}];{}",
            ind,
            ctx.c_type(NodeKind::UTF8),
            name,
            node.str_len,
            oc
        ),
        NodeKind::UTF16 => format!(
            "{}{} {}[{}];{}",
            ind,
            ctx.c_type(NodeKind::UTF16),
            name,
            node.str_len,
            oc
        ),
        NodeKind::Pointer32 | NodeKind::Pointer64 => {
            if node.ref_id != 0 {
                let ref_idx = tree.index_of_id(node.ref_id);
                if ref_idx >= 0 {
                    let target = ctx.name_for(&tree.nodes[ref_idx as usize]);
                    return format!("{}struct {}* {};{}", ind, target, name, oc);
                }
            }
            if is_native_ptr(node.kind, tree.pointer_size) {
                format!("{}void* {};{}", ind, name, oc)
            } else {
                format!("{}{} {};{}", ind, ctx.c_type(node.kind), name, oc)
            }
        }
        NodeKind::FuncPtr32 => format!("{}void (*{})();{}", ind, name, oc),
        NodeKind::FuncPtr64 => format!("{}void (*{})();{}", ind, name, oc),
        _ => format!("{}{} {};{}", ind, ctx.c_type(node.kind), name, oc),
    }
}

/// `emitStructBody(...)` (`generator.cpp:225-377`).
fn emit_struct_body(
    ctx: &mut GenContext,
    struct_id: u64,
    is_union: bool,
    depth: i32,
    base_offset: i32,
) {
    let idx = ctx.tree.index_of_id(struct_id);
    if idx < 0 {
        return;
    }

    let struct_size = ctx.tree.struct_span(struct_id);
    let ind = indent(depth);

    let (children, static_idxs) = ctx.prepare_children(struct_id);

    let mut cursor = 0i32;
    let mut i = 0usize;

    while i < children.len() {
        let child = ctx.tree.nodes[children[i]].clone();
        let child_size = if matches!(child.kind, NodeKind::Struct | NodeKind::Array) {
            ctx.tree.struct_span(child.id)
        } else {
            child.byte_size()
        };

        // Gap/overlap handling (skip for unions).
        if !is_union {
            if child.offset > cursor {
                emit_pad_run_c(ctx, &ind, base_offset, cursor, child.offset - cursor);
            } else if child.offset < cursor {
                ctx.output.push_str(&format!(
                    "{}// WARNING: overlap at offset 0x{:X} (previous field ends at 0x{:X})\n",
                    ind,
                    base_offset + child.offset,
                    base_offset + cursor
                ));
            }
        }

        // Collapse consecutive hex nodes into a single padding array.
        if is_hex_node(child.kind) {
            let run_start = child.offset;
            let mut run_end = child.offset + child_size;
            let mut j = i + 1;
            while j < children.len() {
                let next = &ctx.tree.nodes[children[j]];
                if !is_hex_node(next.kind) {
                    break;
                }
                let next_size = next.byte_size();
                if next.offset < run_end {
                    break;
                }
                run_end = next.offset + next_size;
                j += 1;
            }
            emit_pad_run_c(ctx, &ind, base_offset, run_start, run_end - run_start);
            cursor = run_end;
            i = j;
            continue;
        }

        if child.kind == NodeKind::Struct {
            if child.is_bitfield() && !child.bitfield_members.is_empty() {
                let mut bf_type = ctx.c_type(child.element_kind);
                if bf_type.is_empty() {
                    bf_type = "uint32_t".to_string();
                }
                let field_name = if child.name.is_empty() {
                    String::new()
                } else {
                    format!(" {}", sanitize_ident(&child.name))
                };
                ctx.output.push_str(&format!("{}struct\n", ind));
                ctx.output.push_str(&format!("{}{{\n", ind));
                let bf_ind = indent(depth + 1);
                for m in &child.bitfield_members {
                    ctx.output.push_str(&format!(
                        "{}{} {} : {};{}\n",
                        bf_ind,
                        bf_type,
                        sanitize_ident(&m.name),
                        m.bit_width,
                        offset_comment(base_offset + child.offset, false)
                    ));
                }
                ctx.output.push_str(&format!(
                    "{}}}{};{}\n",
                    ind,
                    field_name,
                    offset_comment(base_offset + child.offset, false)
                ));
            } else if child.struct_type_name.is_empty() {
                // Inline anonymous struct/union.
                let kw = child.resolved_class_keyword().to_string();
                ctx.output.push_str(&format!("{}{}\n", ind, kw));
                ctx.output.push_str(&format!("{}{{\n", ind));
                let child_is_union = kw == "union";
                emit_struct_body(
                    ctx,
                    child.id,
                    child_is_union,
                    depth + 1,
                    base_offset + child.offset,
                );
                let field_name = if child.name.is_empty() {
                    String::new()
                } else {
                    format!(" {}", sanitize_ident(&child.name))
                };
                ctx.output.push_str(&format!(
                    "{}}}{};{}\n",
                    ind,
                    field_name,
                    offset_comment(base_offset + child.offset, false)
                ));
            } else {
                // Named struct — reference by name with struct keyword prefix.
                let mut kw = child.resolved_class_keyword().to_string();
                if kw == "enum" && child.enum_members.is_empty() {
                    kw = "struct".to_string();
                }
                let type_name = ctx.name_for(&child);
                let field_name = sanitize_ident(&child.name);
                ctx.output.push_str(&format!(
                    "{}{} {} {};{}\n",
                    ind,
                    kw,
                    type_name,
                    field_name,
                    offset_comment(base_offset + child.offset, false)
                ));
            }
        } else if child.kind == NodeKind::Array {
            let elem_type_name = ctx.array_elem_struct_name(child.id);
            let field_name = sanitize_ident(&child.name);
            match elem_type_name {
                Some(elem) if !elem.is_empty() => {
                    ctx.output.push_str(&format!(
                        "{}struct {} {}[{}];{}\n",
                        ind,
                        elem,
                        field_name,
                        child.array_len,
                        offset_comment(base_offset + child.offset, false)
                    ));
                }
                _ => {
                    ctx.output.push_str(&format!(
                        "{}{} {}[{}];{}\n",
                        ind,
                        ctx.c_type(child.element_kind),
                        field_name,
                        child.array_len,
                        offset_comment(base_offset + child.offset, false)
                    ));
                }
            }
        } else {
            let line = emit_field(ctx, &child, depth, base_offset);
            ctx.output.push_str(&line);
            ctx.output.push('\n');
        }

        let child_end = child.offset + child_size;
        if child_end > cursor {
            cursor = child_end;
        }
        i += 1;
    }

    // Tail padding (skip for unions).
    if !is_union && cursor < struct_size {
        emit_pad_run_c(ctx, &ind, base_offset, cursor, struct_size - cursor);
    }

    // Static field comments.
    for si in static_idxs {
        let sf = &ctx.tree.nodes[si];
        let sf_type = if sf.struct_type_name.is_empty() {
            ctx.c_type(sf.kind)
        } else {
            sf.struct_type_name.clone()
        };
        let line = format!(
            "{}// static: {} {} @ {}\n",
            ind,
            sf_type,
            sanitize_ident(&sf.name),
            sf.offset_expr
        );
        ctx.output.push_str(&line);
    }
}

/// The `emitPadRun` lambda inside `emitStructBody` (`generator.cpp:237-243`).
fn emit_pad_run_c(ctx: &mut GenContext, ind: &str, base_offset: i32, rel_offset: i32, size: i32) {
    if size <= 0 {
        return;
    }
    let pad = ctx.unique_pad_name();
    ctx.output.push_str(&format!(
        "{}uint8_t {}[0x{:X}];{}\n",
        ind,
        pad,
        size,
        offset_comment(base_offset + rel_offset, false)
    ));
}

/// `emitStruct(GenContext&, uint64_t)` (`generator.cpp:381-457`).
fn emit_struct(ctx: &mut GenContext, struct_id: u64) {
    if ctx.emitted_ids.contains(&struct_id) {
        return;
    }
    if ctx.visiting.contains(&struct_id) {
        return; // cycle
    }
    ctx.visiting.insert(struct_id);

    let idx = ctx.tree.index_of_id(struct_id);
    if idx < 0 {
        ctx.visiting.remove(&struct_id);
        return;
    }

    let node = ctx.tree.nodes[idx as usize].clone();
    if node.kind != NodeKind::Struct && node.kind != NodeKind::Array {
        ctx.visiting.remove(&struct_id);
        return;
    }
    if node.kind == NodeKind::Array {
        ctx.visiting.remove(&struct_id);
        return;
    }

    let type_name = ctx.name_for(&node);
    if ctx.emitted_type_names.contains(&type_name) {
        ctx.emitted_ids.insert(struct_id);
        ctx.visiting.remove(&struct_id);
        return;
    }

    ctx.emitted_ids.insert(struct_id);
    ctx.emitted_type_names.insert(type_name.clone());

    // Forward-declare pointer targets not yet emitted.
    if let Some(kids) = ctx.child_map.get(&struct_id).cloned() {
        for ci in kids {
            let child = &ctx.tree.nodes[ci];
            if is_pointer_kind(child.kind) && child.ref_id != 0 {
                let ref_id = child.ref_id;
                let ri = ctx.tree.index_of_id(ref_id);
                if ri >= 0
                    && !ctx.emitted_ids.contains(&ref_id)
                    && !ctx.forward_declared.contains(&ref_id)
                {
                    let fwd = ctx.name_for(&ctx.tree.nodes[ri as usize]);
                    ctx.output.push_str(&format!("struct {};\n", fwd));
                    ctx.forward_declared.insert(ref_id);
                }
            }
        }
    }

    let struct_size = ctx.tree.struct_span(struct_id);
    let mut kw = node.resolved_class_keyword().to_string();

    // Enum with members.
    if kw == "enum" && !node.enum_members.is_empty() {
        ctx.output.push_str(&format!("enum {} {{\n", type_name));
        for m in &node.enum_members {
            ctx.output
                .push_str(&format!("    {} = {},\n", sanitize_ident(&m.0), m.1));
        }
        ctx.output.push_str("};\n\n");
        ctx.visiting.remove(&struct_id);
        return;
    }

    if kw == "enum" {
        kw = "struct".to_string();
    }

    ctx.output.push_str(&format!("{} {}\n{{\n", kw, type_name));

    emit_struct_body(ctx, struct_id, kw == "union", 1, 0);

    ctx.output
        .push_str(&format!("}};{}\n", offset_comment(struct_size, true)));
    if ctx.emit_asserts {
        ctx.output.push_str(&format!(
            "static_assert(sizeof({0}) == 0x{1:X}, \"Size mismatch for {0}\");\n",
            type_name, struct_size
        ));
    }
    ctx.output.push('\n');

    ctx.visiting.remove(&struct_id);
}

// ═══════════════════════════════════════════════════════════════════
// ── Rust backend ──
// ═══════════════════════════════════════════════════════════════════

/// `emitRustField(...)` (`generator.cpp:554-596`).
fn emit_rust_field(ctx: &GenContext, node: &Node, depth: i32, base_offset: i32) -> String {
    let tree = ctx.tree;
    let ind = indent(depth);
    let name = default_field_name(node);
    let oc = offset_comment(base_offset + node.offset, false);

    match node.kind {
        NodeKind::Vec2 => format!("{}pub {}: [f32; 2],{}", ind, name, oc),
        NodeKind::Vec3 => format!("{}pub {}: [f32; 3],{}", ind, name, oc),
        NodeKind::Vec4 => format!("{}pub {}: [f32; 4],{}", ind, name, oc),
        NodeKind::Mat4x4 => format!("{}pub {}: [[f32; 4]; 4],{}", ind, name, oc),
        NodeKind::UTF8 => format!("{}pub {}: [u8; {}],{}", ind, name, node.str_len, oc),
        NodeKind::UTF16 => format!("{}pub {}: [u16; {}],{}", ind, name, node.str_len, oc),
        NodeKind::Pointer32 | NodeKind::Pointer64 => {
            if node.ref_id != 0 {
                let ref_idx = tree.index_of_id(node.ref_id);
                if ref_idx >= 0 {
                    let target = ctx.name_for(&tree.nodes[ref_idx as usize]);
                    return format!("{}pub {}: *mut {},{}", ind, name, target, oc);
                }
            }
            if is_native_ptr(node.kind, tree.pointer_size) {
                format!("{}pub {}: *mut core::ffi::c_void,{}", ind, name, oc)
            } else {
                format!("{}pub {}: {},{}", ind, name, ctx.rust_type(node.kind), oc)
            }
        }
        NodeKind::FuncPtr32 | NodeKind::FuncPtr64 => {
            format!(
                "{}pub {}: Option<unsafe extern \"C\" fn()>,{}",
                ind, name, oc
            )
        }
        _ => format!("{}pub {}: {},{}", ind, name, ctx.rust_type(node.kind), oc),
    }
}

/// The `emitPadRun` lambda inside `emitRustStructBody` (`generator.cpp:609-615`).
fn emit_pad_run_rust(
    ctx: &mut GenContext,
    ind: &str,
    base_offset: i32,
    rel_offset: i32,
    size: i32,
) {
    if size <= 0 {
        return;
    }
    let pad = ctx.unique_pad_name();
    ctx.output.push_str(&format!(
        "{}pub {}: [u8; 0x{:X}],{}\n",
        ind,
        pad,
        size,
        offset_comment(base_offset + rel_offset, false)
    ));
}

/// `emitRustStructBody(...)` (`generator.cpp:598-729`).
fn emit_rust_struct_body(
    ctx: &mut GenContext,
    struct_id: u64,
    is_union: bool,
    depth: i32,
    base_offset: i32,
) {
    let idx = ctx.tree.index_of_id(struct_id);
    if idx < 0 {
        return;
    }

    let struct_size = ctx.tree.struct_span(struct_id);
    let ind = indent(depth);

    let (children, static_idxs) = ctx.prepare_children(struct_id);

    let mut cursor = 0i32;
    let mut i = 0usize;

    while i < children.len() {
        let child = ctx.tree.nodes[children[i]].clone();
        let child_size = if matches!(child.kind, NodeKind::Struct | NodeKind::Array) {
            ctx.tree.struct_span(child.id)
        } else {
            child.byte_size()
        };

        if !is_union && child.offset > cursor {
            emit_pad_run_rust(ctx, &ind, base_offset, cursor, child.offset - cursor);
        }

        if is_hex_node(child.kind) {
            let run_start = child.offset;
            let mut run_end = child.offset + child_size;
            let mut j = i + 1;
            while j < children.len() {
                let next = &ctx.tree.nodes[children[j]];
                if !is_hex_node(next.kind) {
                    break;
                }
                let next_size = next.byte_size();
                if next.offset < run_end {
                    break;
                }
                run_end = next.offset + next_size;
                j += 1;
            }
            emit_pad_run_rust(ctx, &ind, base_offset, run_start, run_end - run_start);
            cursor = run_end;
            i = j;
            continue;
        }

        if child.kind == NodeKind::Struct {
            if child.is_bitfield() && !child.bitfield_members.is_empty() {
                let mut bf_type = ctx.rust_type(child.element_kind);
                if bf_type.is_empty() {
                    bf_type = "u32".to_string();
                }
                let field_name = sanitize_ident(&if child.name.is_empty() {
                    format!("bitfield_{:02x}", child.offset)
                } else {
                    child.name.clone()
                });
                let bits = bitfield_bits(&child.bitfield_members);
                ctx.output.push_str(&format!(
                    "{}pub {}: {}, // bits: {}{}\n",
                    ind,
                    field_name,
                    bf_type,
                    bits,
                    offset_comment(base_offset + child.offset, false)
                ));
            } else if child.struct_type_name.is_empty() {
                // Rust can't do anonymous inline structs — flatten as byte array.
                let span = ctx.tree.struct_span(child.id);
                let field_name = sanitize_ident(&if child.name.is_empty() {
                    format!("anon_{:02x}", child.offset)
                } else {
                    child.name.clone()
                });
                ctx.output.push_str(&format!(
                    "{}pub {}: [u8; 0x{:X}],{}\n",
                    ind,
                    field_name,
                    span,
                    offset_comment(base_offset + child.offset, false)
                ));
            } else {
                // kw enum→struct fixup computed (unused in text), kept for parity.
                let mut kw = child.resolved_class_keyword().to_string();
                if kw == "enum" && child.enum_members.is_empty() {
                    kw = "struct".to_string();
                }
                let _ = kw;
                let type_name = ctx.name_for(&child);
                let field_name = sanitize_ident(&child.name);
                ctx.output.push_str(&format!(
                    "{}pub {}: {},{}\n",
                    ind,
                    field_name,
                    type_name,
                    offset_comment(base_offset + child.offset, false)
                ));
            }
        } else if child.kind == NodeKind::Array {
            let elem_type_name = ctx.array_elem_struct_name(child.id);
            let field_name = sanitize_ident(&child.name);
            match elem_type_name {
                Some(elem) if !elem.is_empty() => {
                    ctx.output.push_str(&format!(
                        "{}pub {}: [{}; {}],{}\n",
                        ind,
                        field_name,
                        elem,
                        child.array_len,
                        offset_comment(base_offset + child.offset, false)
                    ));
                }
                _ => {
                    ctx.output.push_str(&format!(
                        "{}pub {}: [{}; {}],{}\n",
                        ind,
                        field_name,
                        ctx.rust_type(child.element_kind),
                        child.array_len,
                        offset_comment(base_offset + child.offset, false)
                    ));
                }
            }
        } else {
            let line = emit_rust_field(ctx, &child, depth, base_offset);
            ctx.output.push_str(&line);
            ctx.output.push('\n');
        }

        let child_end = child.offset + child_size;
        if child_end > cursor {
            cursor = child_end;
        }
        i += 1;
    }

    if !is_union && cursor < struct_size {
        emit_pad_run_rust(ctx, &ind, base_offset, cursor, struct_size - cursor);
    }

    for si in static_idxs {
        let sf = &ctx.tree.nodes[si];
        let sf_type = if sf.struct_type_name.is_empty() {
            ctx.rust_type(sf.kind)
        } else {
            sf.struct_type_name.clone()
        };
        let line = format!(
            "{}// static: {} {} @ {}\n",
            ind,
            sf_type,
            sanitize_ident(&sf.name),
            sf.offset_expr
        );
        ctx.output.push_str(&line);
    }
}

/// Join `name:bits` for the bitfield comment (`generator.cpp:660-662`).
fn bitfield_bits(members: &[BitfieldMember]) -> String {
    members
        .iter()
        .map(|m| format!("{}:{}", sanitize_ident(&m.name), m.bit_width))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `emitRustStruct(GenContext&, uint64_t)` (`generator.cpp:731-787`).
fn emit_rust_struct(ctx: &mut GenContext, struct_id: u64) {
    if ctx.emitted_ids.contains(&struct_id) {
        return;
    }
    if ctx.visiting.contains(&struct_id) {
        return;
    }
    ctx.visiting.insert(struct_id);

    let idx = ctx.tree.index_of_id(struct_id);
    if idx < 0 {
        ctx.visiting.remove(&struct_id);
        return;
    }

    let node = ctx.tree.nodes[idx as usize].clone();
    if node.kind != NodeKind::Struct {
        ctx.visiting.remove(&struct_id);
        return;
    }

    let type_name = ctx.name_for(&node);
    if ctx.emitted_type_names.contains(&type_name) {
        ctx.emitted_ids.insert(struct_id);
        ctx.visiting.remove(&struct_id);
        return;
    }

    ctx.emitted_ids.insert(struct_id);
    ctx.emitted_type_names.insert(type_name.clone());
    let struct_size = ctx.tree.struct_span(struct_id);

    let kw = node.resolved_class_keyword().to_string();

    // Enum with members.
    if kw == "enum" && !node.enum_members.is_empty() {
        ctx.output
            .push_str(&format!("#[repr(i64)]\npub enum {} {{\n", type_name));
        for m in &node.enum_members {
            ctx.output
                .push_str(&format!("    {} = {},\n", sanitize_ident(&m.0), m.1));
        }
        ctx.output.push_str("}\n\n");
        ctx.visiting.remove(&struct_id);
        return;
    }

    let is_union = kw == "union";

    if is_union {
        ctx.output.push_str(&format!(
            "#[repr(C)]\n#[derive(Copy, Clone)]\n#[allow(dead_code)]\npub union {} {{\n",
            type_name
        ));
    } else {
        ctx.output.push_str(&format!(
            "#[repr(C)]\n#[derive(Debug)]\n#[allow(dead_code)]\npub struct {} {{\n",
            type_name
        ));
    }

    emit_rust_struct_body(ctx, struct_id, is_union, 1, 0);

    ctx.output
        .push_str(&format!("}}{}\n", offset_comment(struct_size, true)));
    if ctx.emit_asserts {
        ctx.output.push_str(&format!(
            "const _: () = assert!(core::mem::size_of::<{}>() == 0x{:X});\n",
            type_name, struct_size
        ));
    }
    ctx.output.push('\n');

    ctx.visiting.remove(&struct_id);
}

// ═══════════════════════════════════════════════════════════════════
// ── #define offsets backend ──
// ═══════════════════════════════════════════════════════════════════

/// `emitDefinesForStruct(...)` (`generator.cpp:793-846`).
fn emit_defines_for_struct(ctx: &mut GenContext, struct_id: u64, prefix: &str, base_offset: i32) {
    let idx = ctx.tree.index_of_id(struct_id);
    if idx < 0 {
        return;
    }

    let node = ctx.tree.nodes[idx as usize].clone();
    let type_name = if prefix.is_empty() {
        ctx.name_for(&node)
    } else {
        prefix.to_string()
    };
    let kw = node.resolved_class_keyword().to_string();

    // Enum with members.
    if kw == "enum" && !node.enum_members.is_empty() {
        ctx.output.push_str(&format!("// {} (enum)\n", type_name));
        for m in &node.enum_members {
            ctx.output.push_str(&format!(
                "#define {}_{} {}\n",
                type_name,
                sanitize_ident(&m.0),
                m.1
            ));
        }
        ctx.output.push('\n');
        return;
    }

    let struct_size = ctx.tree.struct_span(struct_id);
    ctx.output
        .push_str(&format!("// {} (0x{:X} bytes)\n", type_name, struct_size));

    let mut children: Vec<usize> = ctx.child_map.get(&struct_id).cloned().unwrap_or_default();
    children.sort_by_key(|&a| ctx.tree.nodes[a].offset);

    for ci in children {
        let child = ctx.tree.nodes[ci].clone();
        if child.is_static {
            continue;
        }
        if is_hex_node(child.kind) {
            continue;
        }

        let field_name = default_field_name(&child);
        let abs_offset = base_offset + child.offset;

        ctx.output.push_str(&format!(
            "#define {}_{} 0x{:X}\n",
            type_name, field_name, abs_offset
        ));

        // Recurse into named sub-structs.
        if child.kind == NodeKind::Struct
            && !child.struct_type_name.is_empty()
            && child.class_keyword != "bitfield"
        {
            emit_defines_for_struct(
                ctx,
                child.id,
                &format!("{}_{}", type_name, field_name),
                abs_offset,
            );
        }
    }
    ctx.output.push('\n');
}

// ═══════════════════════════════════════════════════════════════════
// ── C# backend ──
// ═══════════════════════════════════════════════════════════════════

/// `emitCSharpStructBody(...)` (`generator.cpp:899-1033`).
fn emit_csharp_struct_body(
    ctx: &mut GenContext,
    struct_id: u64,
    _is_union: bool,
    depth: i32,
    base_offset: i32,
) {
    let idx = ctx.tree.index_of_id(struct_id);
    if idx < 0 {
        return;
    }

    let ind = indent(depth);
    let (children, static_idxs) = ctx.prepare_children(struct_id);

    // C# uses [FieldOffset(N)] — no manual padding.
    for ci in children {
        let child = ctx.tree.nodes[ci].clone();
        if is_hex_node(child.kind) {
            continue;
        }

        let abs = base_offset + child.offset;
        let abs_hex = format!("{:X}", abs);
        let name = default_field_name(&child);
        let oc = offset_comment(abs, false);

        if child.kind == NodeKind::Struct {
            if child.is_bitfield() && !child.bitfield_members.is_empty() {
                let mut bf_type = ctx.cs_type(child.element_kind);
                if bf_type.is_empty() {
                    bf_type = "uint".to_string();
                }
                let bits = bitfield_bits(&child.bitfield_members);
                ctx.output.push_str(&format!(
                    "{}[FieldOffset(0x{})] public {} {}; // bits: {}{}\n",
                    ind, abs_hex, bf_type, name, bits, oc
                ));
            } else if child.struct_type_name.is_empty() {
                let span = ctx.tree.struct_span(child.id);
                ctx.output.push_str(&format!(
                    "{}[FieldOffset(0x{})] public fixed byte {}[0x{:X}];{}\n",
                    ind, abs_hex, name, span, oc
                ));
            } else {
                let type_name = ctx.name_for(&child);
                ctx.output.push_str(&format!(
                    "{}[FieldOffset(0x{})] public {} {};{}\n",
                    ind, abs_hex, type_name, name, oc
                ));
            }
        } else if child.kind == NodeKind::Array {
            let elem_type_name = ctx.array_elem_struct_name(child.id);
            match elem_type_name {
                Some(elem) if !elem.is_empty() => {
                    ctx.output.push_str(&format!(
                        "{}[FieldOffset(0x{})] [MarshalAs(UnmanagedType.ByValArray, SizeConst = {})] public {}[] {};{}\n",
                        ind, abs_hex, child.array_len, elem, name, oc
                    ));
                }
                _ => {
                    let elem_type = ctx.cs_type(child.element_kind);
                    ctx.output.push_str(&format!(
                        "{}[FieldOffset(0x{})] public fixed {} {}[{}];{}\n",
                        ind, abs_hex, elem_type, name, child.array_len, oc
                    ));
                }
            }
        } else {
            // Primitive fields.
            match child.kind {
                NodeKind::Vec2 => ctx.output.push_str(&format!(
                    "{}[FieldOffset(0x{})] public fixed float {}[2];{}\n",
                    ind, abs_hex, name, oc
                )),
                NodeKind::Vec3 => ctx.output.push_str(&format!(
                    "{}[FieldOffset(0x{})] public fixed float {}[3];{}\n",
                    ind, abs_hex, name, oc
                )),
                NodeKind::Vec4 => ctx.output.push_str(&format!(
                    "{}[FieldOffset(0x{})] public fixed float {}[4];{}\n",
                    ind, abs_hex, name, oc
                )),
                NodeKind::Mat4x4 => ctx.output.push_str(&format!(
                    "{}[FieldOffset(0x{})] public fixed float {}[16];{}\n",
                    ind, abs_hex, name, oc
                )),
                NodeKind::UTF8 => ctx.output.push_str(&format!(
                    "{}[FieldOffset(0x{})] public fixed byte {}[{}];{}\n",
                    ind, abs_hex, name, child.str_len, oc
                )),
                NodeKind::UTF16 => ctx.output.push_str(&format!(
                    "{}[FieldOffset(0x{})] public fixed char {}[{}];{}\n",
                    ind, abs_hex, name, child.str_len, oc
                )),
                NodeKind::Pointer32 | NodeKind::Pointer64 => {
                    if is_native_ptr(child.kind, ctx.tree.pointer_size) {
                        ctx.output.push_str(&format!(
                            "{}[FieldOffset(0x{})] public IntPtr {};{}\n",
                            ind, abs_hex, name, oc
                        ));
                    } else {
                        ctx.output.push_str(&format!(
                            "{}[FieldOffset(0x{})] public {} {};{}\n",
                            ind,
                            abs_hex,
                            ctx.cs_type(child.kind),
                            name,
                            oc
                        ));
                    }
                }
                NodeKind::FuncPtr32 | NodeKind::FuncPtr64 => ctx.output.push_str(&format!(
                    "{}[FieldOffset(0x{})] public IntPtr {}; // fn ptr{}\n",
                    ind, abs_hex, name, oc
                )),
                _ => ctx.output.push_str(&format!(
                    "{}[FieldOffset(0x{})] public {} {};{}\n",
                    ind,
                    abs_hex,
                    ctx.cs_type(child.kind),
                    name,
                    oc
                )),
            }
        }
    }

    for si in static_idxs {
        let sf = &ctx.tree.nodes[si];
        let sf_type = if sf.struct_type_name.is_empty() {
            ctx.cs_type(sf.kind)
        } else {
            sf.struct_type_name.clone()
        };
        let line = format!(
            "{}// static: {} {} @ {}\n",
            ind,
            sf_type,
            sanitize_ident(&sf.name),
            sf.offset_expr
        );
        ctx.output.push_str(&line);
    }
}

/// `emitCSharpStruct(GenContext&, uint64_t)` (`generator.cpp:1035-1085`).
fn emit_csharp_struct(ctx: &mut GenContext, struct_id: u64) {
    if ctx.emitted_ids.contains(&struct_id) {
        return;
    }
    if ctx.visiting.contains(&struct_id) {
        return;
    }
    ctx.visiting.insert(struct_id);

    let idx = ctx.tree.index_of_id(struct_id);
    if idx < 0 {
        ctx.visiting.remove(&struct_id);
        return;
    }

    let node = ctx.tree.nodes[idx as usize].clone();
    if node.kind != NodeKind::Struct {
        ctx.visiting.remove(&struct_id);
        return;
    }

    let type_name = ctx.name_for(&node);
    if ctx.emitted_type_names.contains(&type_name) {
        ctx.emitted_ids.insert(struct_id);
        ctx.visiting.remove(&struct_id);
        return;
    }

    ctx.emitted_ids.insert(struct_id);
    ctx.emitted_type_names.insert(type_name.clone());
    let struct_size = ctx.tree.struct_span(struct_id);

    let kw = node.resolved_class_keyword().to_string();

    // Enum with members.
    if kw == "enum" && !node.enum_members.is_empty() {
        ctx.output
            .push_str(&format!("public enum {} : long\n{{\n", type_name));
        for m in &node.enum_members {
            ctx.output
                .push_str(&format!("    {} = {},\n", sanitize_ident(&m.0), m.1));
        }
        ctx.output.push_str("}\n\n");
        ctx.visiting.remove(&struct_id);
        return;
    }

    let is_union = kw == "union";

    ctx.output.push_str(&format!(
        "[StructLayout(LayoutKind.Explicit, Size = 0x{:X})]\n",
        struct_size
    ));
    ctx.output
        .push_str(&format!("public unsafe struct {}\n{{\n", type_name));

    emit_csharp_struct_body(ctx, struct_id, is_union, 1, 0);

    ctx.output
        .push_str(&format!("}}{}\n\n", offset_comment(struct_size, true)));

    ctx.visiting.remove(&struct_id);
}

// ═══════════════════════════════════════════════════════════════════
// ── Python ctypes backend ──
// ═══════════════════════════════════════════════════════════════════

/// The `emitPadField` lambda inside `emitPythonStructBody` (`generator.cpp:1140-1146`).
fn emit_pad_field_py(
    ctx: &mut GenContext,
    ind: &str,
    base_offset: i32,
    rel_offset: i32,
    size: i32,
) {
    if size <= 0 {
        return;
    }
    let pad = ctx.unique_pad_name();
    ctx.output.push_str(&format!(
        "{}(\"{}\", ctypes.c_uint8 * 0x{:X}),{}\n",
        ind,
        pad,
        size,
        offset_comment(base_offset + rel_offset, false)
    ));
}

/// `emitPythonStructBody(...)` (`generator.cpp:1129-1300`).
fn emit_python_struct_body(ctx: &mut GenContext, struct_id: u64, is_union: bool, base_offset: i32) {
    let idx = ctx.tree.index_of_id(struct_id);
    if idx < 0 {
        return;
    }

    let struct_size = ctx.tree.struct_span(struct_id);
    let ind = "        "; // 2 levels for inside _fields_

    let (children, _static_idxs) = ctx.prepare_children(struct_id);

    let mut cursor = 0i32;
    let mut i = 0usize;

    while i < children.len() {
        let child = ctx.tree.nodes[children[i]].clone();
        let child_size = if matches!(child.kind, NodeKind::Struct | NodeKind::Array) {
            ctx.tree.struct_span(child.id)
        } else {
            child.byte_size()
        };

        if !is_union && child.offset > cursor {
            emit_pad_field_py(ctx, ind, base_offset, cursor, child.offset - cursor);
        }

        if is_hex_node(child.kind) {
            let run_start = child.offset;
            let mut run_end = child.offset + child_size;
            let mut j = i + 1;
            while j < children.len() {
                let next = &ctx.tree.nodes[children[j]];
                if !is_hex_node(next.kind) {
                    break;
                }
                let next_size = next.byte_size();
                if next.offset < run_end {
                    break;
                }
                run_end = next.offset + next_size;
                j += 1;
            }
            emit_pad_field_py(ctx, ind, base_offset, run_start, run_end - run_start);
            cursor = run_end;
            i = j;
            continue;
        }

        let abs_offset = base_offset + child.offset;
        let name = default_field_name(&child);
        let oc = offset_comment(abs_offset, false);

        if child.kind == NodeKind::Struct {
            if child.is_bitfield() && !child.bitfield_members.is_empty() {
                let mut bf_type = py_type_name(child.element_kind).to_string();
                if bf_type.is_empty() {
                    bf_type = "ctypes.c_uint32".to_string();
                }
                let bits = bitfield_bits(&child.bitfield_members);
                ctx.output.push_str(&format!(
                    "{}(\"{}\", {}), # bits: {}{}\n",
                    ind, name, bf_type, bits, oc
                ));
            } else if child.struct_type_name.is_empty() {
                let span = ctx.tree.struct_span(child.id);
                ctx.output.push_str(&format!(
                    "{}(\"{}\", ctypes.c_uint8 * 0x{:X}),{}\n",
                    ind, name, span, oc
                ));
            } else {
                let type_name = ctx.name_for(&child);
                ctx.output
                    .push_str(&format!("{}(\"{}\", {}),{}\n", ind, name, type_name, oc));
            }
        } else if child.kind == NodeKind::Array {
            let elem_type_name = ctx.array_elem_struct_name(child.id);
            match elem_type_name {
                Some(elem) if !elem.is_empty() => {
                    ctx.output.push_str(&format!(
                        "{}(\"{}\", {} * {}),{}\n",
                        ind, name, elem, child.array_len, oc
                    ));
                }
                _ => {
                    ctx.output.push_str(&format!(
                        "{}(\"{}\", {} * {}),{}\n",
                        ind,
                        name,
                        py_type_name(child.element_kind),
                        child.array_len,
                        oc
                    ));
                }
            }
        } else {
            // Primitive fields.
            match child.kind {
                NodeKind::Vec2 => ctx.output.push_str(&format!(
                    "{}(\"{}\", ctypes.c_float * 2),{}\n",
                    ind, name, oc
                )),
                NodeKind::Vec3 => ctx.output.push_str(&format!(
                    "{}(\"{}\", ctypes.c_float * 3),{}\n",
                    ind, name, oc
                )),
                NodeKind::Vec4 => ctx.output.push_str(&format!(
                    "{}(\"{}\", ctypes.c_float * 4),{}\n",
                    ind, name, oc
                )),
                NodeKind::Mat4x4 => ctx.output.push_str(&format!(
                    "{}(\"{}\", (ctypes.c_float * 4) * 4),{}\n",
                    ind, name, oc
                )),
                NodeKind::UTF8 => ctx.output.push_str(&format!(
                    "{}(\"{}\", ctypes.c_char * {}),{}\n",
                    ind, name, child.str_len, oc
                )),
                NodeKind::UTF16 => ctx.output.push_str(&format!(
                    "{}(\"{}\", ctypes.c_wchar * {}),{}\n",
                    ind, name, child.str_len, oc
                )),
                NodeKind::Pointer32 | NodeKind::Pointer64 => {
                    let mut handled = false;
                    if child.ref_id != 0 {
                        let ref_idx = ctx.tree.index_of_id(child.ref_id);
                        if ref_idx >= 0 {
                            let target = ctx.name_for(&ctx.tree.nodes[ref_idx as usize]);
                            ctx.output.push_str(&format!(
                                "{}(\"{}\", ctypes.POINTER({})),{}\n",
                                ind, name, target, oc
                            ));
                            handled = true;
                        }
                    }
                    if !handled {
                        if is_native_ptr(child.kind, ctx.tree.pointer_size) {
                            ctx.output.push_str(&format!(
                                "{}(\"{}\", ctypes.c_void_p),{}\n",
                                ind, name, oc
                            ));
                        } else {
                            ctx.output.push_str(&format!(
                                "{}(\"{}\", {}),{}\n",
                                ind,
                                name,
                                py_type_name(child.kind),
                                oc
                            ));
                        }
                    }
                }
                NodeKind::FuncPtr32 | NodeKind::FuncPtr64 => ctx.output.push_str(&format!(
                    "{}(\"{}\", ctypes.CFUNCTYPE(None)),{}\n",
                    ind, name, oc
                )),
                _ => ctx.output.push_str(&format!(
                    "{}(\"{}\", {}),{}\n",
                    ind,
                    name,
                    py_type_name(child.kind),
                    oc
                )),
            }
        }

        let child_end = child.offset + child_size;
        if child_end > cursor {
            cursor = child_end;
        }
        i += 1;
    }

    // Tail padding.
    if !is_union && cursor < struct_size {
        emit_pad_field_py(ctx, ind, base_offset, cursor, struct_size - cursor);
    }
}

/// `emitPythonStruct(GenContext&, uint64_t)` (`generator.cpp:1302-1359`).
fn emit_python_struct(ctx: &mut GenContext, struct_id: u64) {
    if ctx.emitted_ids.contains(&struct_id) {
        return;
    }
    if ctx.visiting.contains(&struct_id) {
        return;
    }
    ctx.visiting.insert(struct_id);

    let idx = ctx.tree.index_of_id(struct_id);
    if idx < 0 {
        ctx.visiting.remove(&struct_id);
        return;
    }

    let node = ctx.tree.nodes[idx as usize].clone();
    if node.kind != NodeKind::Struct {
        ctx.visiting.remove(&struct_id);
        return;
    }

    let type_name = ctx.name_for(&node);
    if ctx.emitted_type_names.contains(&type_name) {
        ctx.emitted_ids.insert(struct_id);
        ctx.visiting.remove(&struct_id);
        return;
    }

    ctx.emitted_ids.insert(struct_id);
    ctx.emitted_type_names.insert(type_name.clone());
    let struct_size = ctx.tree.struct_span(struct_id);

    let kw = node.resolved_class_keyword().to_string();

    // Enum with members.
    if kw == "enum" && !node.enum_members.is_empty() {
        ctx.output.push_str(&format!(
            "class {}:  # enum\n    __slots__ = ()\n",
            type_name
        ));
        for m in &node.enum_members {
            ctx.output
                .push_str(&format!("    {} = {}\n", sanitize_ident(&m.0), m.1));
        }
        ctx.output.push('\n');
        ctx.visiting.remove(&struct_id);
        return;
    }

    let is_union = kw == "union";
    let base_class = if is_union {
        "ctypes.Union"
    } else {
        "ctypes.Structure"
    };

    ctx.output.push_str(&format!(
        "class {}({}):{}\n",
        type_name,
        base_class,
        offset_comment(struct_size, true)
    ));
    ctx.output.push_str("    _fields_ = [\n");

    emit_python_struct_body(ctx, struct_id, is_union, 0);

    ctx.output.push_str("    ]\n");

    // Static field comments.
    let static_idxs = ctx.prepare_children(struct_id).1;
    for si in static_idxs {
        let sf = &ctx.tree.nodes[si];
        let line = format!(
            "    # static: {} {} @ {}\n",
            py_type_name(sf.kind),
            sanitize_ident(&sf.name),
            sf.offset_expr
        );
        ctx.output.push_str(&line);
    }
    ctx.output.push('\n');

    ctx.visiting.remove(&struct_id);
}

// ═══════════════════════════════════════════════════════════════════
// ── Reachable struct collector (`generator.cpp:1368-1402`) ──
// ═══════════════════════════════════════════════════════════════════

/// `collectReachableStructs(...)` (`generator.cpp:1368-1402`). Post-order DFS;
/// dependencies first, root last.
fn collect_reachable_structs(
    tree: &NodeTree,
    child_map: &HashMap<u64, Vec<usize>>,
    root_id: u64,
) -> Vec<u64> {
    let mut result: Vec<u64> = Vec::new();
    let mut visited: HashSet<u64> = HashSet::new();
    reachable_walk(tree, child_map, root_id, &mut visited, &mut result);
    result
}

fn reachable_walk(
    tree: &NodeTree,
    child_map: &HashMap<u64, Vec<usize>>,
    id: u64,
    visited: &mut HashSet<u64>,
    result: &mut Vec<u64>,
) {
    if !visited.insert(id) {
        return; // already visited
    }

    let idx = tree.index_of_id(id);
    if idx < 0 {
        return;
    }
    if tree.nodes[idx as usize].kind != NodeKind::Struct {
        return;
    }

    // Walk children first so dependencies come before the parent.
    if let Some(kids) = child_map.get(&id) {
        for &ci in kids {
            let child = &tree.nodes[ci];
            if child.kind == NodeKind::Struct && !child.struct_type_name.is_empty() {
                reachable_walk(tree, child_map, child.id, visited, result);
            }
            if (child.kind == NodeKind::Pointer32 || child.kind == NodeKind::Pointer64)
                && child.ref_id != 0
            {
                reachable_walk(tree, child_map, child.ref_id, visited, result);
            }
            if child.kind == NodeKind::Array {
                if let Some(array_kids) = child_map.get(&child.id) {
                    for &ak in array_kids {
                        if tree.nodes[ak].kind == NodeKind::Struct {
                            let aid = tree.nodes[ak].id;
                            reachable_walk(tree, child_map, aid, visited, result);
                        }
                    }
                }
            }
        }
    }
    result.push(id);
}

// ═══════════════════════════════════════════════════════════════════
// ── Public API ──
// ═══════════════════════════════════════════════════════════════════

/// `codeFormatName(CodeFormat)` (`generator.cpp:1408-1417`).
pub fn code_format_name(fmt: CodeFormat) -> &'static str {
    match fmt {
        CodeFormat::CppHeader => "C/C++",
        CodeFormat::RustStruct => "Rust",
        CodeFormat::DefineOffsets => "#define",
        CodeFormat::CSharpStruct => "C#",
        CodeFormat::PythonCtypes => "Python",
    }
}

/// `codeFormatFileFilter(CodeFormat)` (`generator.cpp:1419-1428`).
pub fn code_format_file_filter(fmt: CodeFormat) -> &'static str {
    match fmt {
        CodeFormat::CppHeader => "C++ Header (*.h);;All Files (*)",
        CodeFormat::RustStruct => "Rust Source (*.rs);;All Files (*)",
        CodeFormat::DefineOffsets => "C Header (*.h);;All Files (*)",
        CodeFormat::CSharpStruct => "C# Source (*.cs);;All Files (*)",
        CodeFormat::PythonCtypes => "Python Source (*.py);;All Files (*)",
    }
}

/// `codeScopeName(CodeScope)` (`generator.cpp:1430-1437`).
pub fn code_scope_name(scope: CodeScope) -> &'static str {
    match scope {
        CodeScope::Current => "Current",
        CodeScope::WithChildren => "Current + Deps",
        CodeScope::FullSdk => "Full SDK",
    }
}

// ── C/C++ public API (`generator.cpp:1439-1498`) ──

/// `renderCpp(...)` (`generator.cpp:1439-1457`).
pub fn render_cpp(
    tree: &NodeTree,
    root_struct_id: u64,
    type_aliases: Option<&TypeAliases>,
    emit_asserts: bool,
) -> String {
    let idx = tree.index_of_id(root_struct_id);
    if idx < 0 {
        return String::new();
    }
    if tree.nodes[idx as usize].kind != NodeKind::Struct {
        return String::new();
    }

    let mut ctx = GenContext::new(tree, type_aliases, emit_asserts);
    ctx.assign_unique_names();
    ctx.output.push_str("#pragma once\n#include <cstdint>\n\n");
    emit_struct(&mut ctx, root_struct_id);
    align_comments(&ctx.output)
}

/// `renderCppTree(...)` (`generator.cpp:1459-1476`).
pub fn render_cpp_tree(
    tree: &NodeTree,
    root_struct_id: u64,
    type_aliases: Option<&TypeAliases>,
    emit_asserts: bool,
) -> String {
    let idx = tree.index_of_id(root_struct_id);
    if idx < 0 {
        return String::new();
    }
    if tree.nodes[idx as usize].kind != NodeKind::Struct {
        return String::new();
    }

    let mut ctx = GenContext::new(tree, type_aliases, emit_asserts);
    ctx.assign_unique_names();
    ctx.output.push_str("#pragma once\n#include <cstdint>\n\n");

    for sid in collect_reachable_structs(tree, &ctx.child_map, root_struct_id) {
        emit_struct(&mut ctx, sid);
    }
    align_comments(&ctx.output)
}

/// `renderCppAll(...)` (`generator.cpp:1478-1498`).
pub fn render_cpp_all(
    tree: &NodeTree,
    type_aliases: Option<&TypeAliases>,
    emit_asserts: bool,
) -> String {
    let mut ctx = GenContext::new(tree, type_aliases, emit_asserts);
    ctx.assign_unique_names();
    ctx.output.push_str("#pragma once\n#include <cstdint>\n\n");

    let mut roots: Vec<usize> = ctx.child_map.get(&0).cloned().unwrap_or_default();
    roots.sort_by_key(|&i| tree.nodes[i].offset);
    for ri in roots {
        if tree.nodes[ri].kind == NodeKind::Struct {
            let id = tree.nodes[ri].id;
            emit_struct(&mut ctx, id);
        }
    }
    align_comments(&ctx.output)
}

// ── Rust public API (`generator.cpp:1502-1553`) ──

/// `renderRust(...)` (`generator.cpp:1502-1515`).
pub fn render_rust(
    tree: &NodeTree,
    root_struct_id: u64,
    type_aliases: Option<&TypeAliases>,
    emit_asserts: bool,
) -> String {
    let idx = tree.index_of_id(root_struct_id);
    if idx < 0 {
        return String::new();
    }
    if tree.nodes[idx as usize].kind != NodeKind::Struct {
        return String::new();
    }

    let mut ctx = GenContext::new(tree, type_aliases, emit_asserts);
    ctx.assign_unique_names();
    ctx.output.push_str("// Generated by Reclass 2027\n\n");
    emit_rust_struct(&mut ctx, root_struct_id);
    align_comments(&ctx.output)
}

/// `renderRustTree(...)` (`generator.cpp:1517-1534`).
pub fn render_rust_tree(
    tree: &NodeTree,
    root_struct_id: u64,
    type_aliases: Option<&TypeAliases>,
    emit_asserts: bool,
) -> String {
    let idx = tree.index_of_id(root_struct_id);
    if idx < 0 {
        return String::new();
    }
    if tree.nodes[idx as usize].kind != NodeKind::Struct {
        return String::new();
    }

    let mut ctx = GenContext::new(tree, type_aliases, emit_asserts);
    ctx.assign_unique_names();
    ctx.output.push_str("// Generated by Reclass 2027\n\n");

    for sid in collect_reachable_structs(tree, &ctx.child_map, root_struct_id) {
        emit_rust_struct(&mut ctx, sid);
    }
    align_comments(&ctx.output)
}

/// `renderRustAll(...)` (`generator.cpp:1536-1553`).
pub fn render_rust_all(
    tree: &NodeTree,
    type_aliases: Option<&TypeAliases>,
    emit_asserts: bool,
) -> String {
    let mut ctx = GenContext::new(tree, type_aliases, emit_asserts);
    ctx.assign_unique_names();
    ctx.output.push_str("// Generated by Reclass 2027\n\n");

    let mut roots: Vec<usize> = ctx.child_map.get(&0).cloned().unwrap_or_default();
    roots.sort_by_key(|&i| tree.nodes[i].offset);
    for ri in roots {
        if tree.nodes[ri].kind == NodeKind::Struct {
            let id = tree.nodes[ri].id;
            emit_rust_struct(&mut ctx, id);
        }
    }
    align_comments(&ctx.output)
}

// ── #define public API (`generator.cpp:1557-1602`) ──

/// `renderDefines(...)` (`generator.cpp:1557-1568`). Returns raw output (no
/// `align_comments`; the `#define` text contains no markers).
pub fn render_defines(tree: &NodeTree, root_struct_id: u64) -> String {
    let idx = tree.index_of_id(root_struct_id);
    if idx < 0 {
        return String::new();
    }
    if tree.nodes[idx as usize].kind != NodeKind::Struct {
        return String::new();
    }

    let mut ctx = GenContext::new(tree, None, false);
    ctx.assign_unique_names();
    ctx.output.push_str("#pragma once\n#include <cstdint>\n\n");
    emit_defines_for_struct(&mut ctx, root_struct_id, "", 0);
    ctx.output
}

/// `renderDefinesTree(...)` (`generator.cpp:1570-1585`).
pub fn render_defines_tree(tree: &NodeTree, root_struct_id: u64) -> String {
    let idx = tree.index_of_id(root_struct_id);
    if idx < 0 {
        return String::new();
    }
    if tree.nodes[idx as usize].kind != NodeKind::Struct {
        return String::new();
    }

    let mut ctx = GenContext::new(tree, None, false);
    ctx.assign_unique_names();
    ctx.output.push_str("#pragma once\n#include <cstdint>\n\n");

    for sid in collect_reachable_structs(tree, &ctx.child_map, root_struct_id) {
        emit_defines_for_struct(&mut ctx, sid, "", 0);
    }
    ctx.output
}

/// `renderDefinesAll(...)` (`generator.cpp:1587-1602`).
pub fn render_defines_all(tree: &NodeTree) -> String {
    let mut ctx = GenContext::new(tree, None, false);
    ctx.assign_unique_names();
    ctx.output.push_str("#pragma once\n#include <cstdint>\n\n");

    let mut roots: Vec<usize> = ctx.child_map.get(&0).cloned().unwrap_or_default();
    roots.sort_by_key(|&i| tree.nodes[i].offset);
    for ri in roots {
        if tree.nodes[ri].kind == NodeKind::Struct {
            let id = tree.nodes[ri].id;
            emit_defines_for_struct(&mut ctx, id, "", 0);
        }
    }
    ctx.output
}

// ── C# public API (`generator.cpp:1606-1657`) ──

/// `renderCSharp(...)` (`generator.cpp:1606-1619`).
pub fn render_csharp(
    tree: &NodeTree,
    root_struct_id: u64,
    type_aliases: Option<&TypeAliases>,
    emit_asserts: bool,
) -> String {
    let idx = tree.index_of_id(root_struct_id);
    if idx < 0 {
        return String::new();
    }
    if tree.nodes[idx as usize].kind != NodeKind::Struct {
        return String::new();
    }

    let mut ctx = GenContext::new(tree, type_aliases, emit_asserts);
    ctx.assign_unique_names();
    ctx.output
        .push_str("using System.Runtime.InteropServices;\n#nullable disable\n\n");
    emit_csharp_struct(&mut ctx, root_struct_id);
    align_comments(&ctx.output)
}

/// `renderCSharpTree(...)` (`generator.cpp:1621-1638`).
pub fn render_csharp_tree(
    tree: &NodeTree,
    root_struct_id: u64,
    type_aliases: Option<&TypeAliases>,
    emit_asserts: bool,
) -> String {
    let idx = tree.index_of_id(root_struct_id);
    if idx < 0 {
        return String::new();
    }
    if tree.nodes[idx as usize].kind != NodeKind::Struct {
        return String::new();
    }

    let mut ctx = GenContext::new(tree, type_aliases, emit_asserts);
    ctx.assign_unique_names();
    ctx.output
        .push_str("using System.Runtime.InteropServices;\n#nullable disable\n\n");

    for sid in collect_reachable_structs(tree, &ctx.child_map, root_struct_id) {
        emit_csharp_struct(&mut ctx, sid);
    }
    align_comments(&ctx.output)
}

/// `renderCSharpAll(...)` (`generator.cpp:1640-1657`).
pub fn render_csharp_all(
    tree: &NodeTree,
    type_aliases: Option<&TypeAliases>,
    emit_asserts: bool,
) -> String {
    let mut ctx = GenContext::new(tree, type_aliases, emit_asserts);
    ctx.assign_unique_names();
    ctx.output
        .push_str("using System.Runtime.InteropServices;\n#nullable disable\n\n");

    let mut roots: Vec<usize> = ctx.child_map.get(&0).cloned().unwrap_or_default();
    roots.sort_by_key(|&i| tree.nodes[i].offset);
    for ri in roots {
        if tree.nodes[ri].kind == NodeKind::Struct {
            let id = tree.nodes[ri].id;
            emit_csharp_struct(&mut ctx, id);
        }
    }
    align_comments(&ctx.output)
}

// ── Python public API (`generator.cpp:1661-1706`) ──

/// `renderPython(...)` (`generator.cpp:1661-1672`).
pub fn render_python(tree: &NodeTree, root_struct_id: u64) -> String {
    let idx = tree.index_of_id(root_struct_id);
    if idx < 0 {
        return String::new();
    }
    if tree.nodes[idx as usize].kind != NodeKind::Struct {
        return String::new();
    }

    let mut ctx = GenContext::new(tree, None, false);
    ctx.assign_unique_names();
    ctx.output.push_str("import ctypes\n\n");
    emit_python_struct(&mut ctx, root_struct_id);
    align_comments(&ctx.output)
}

/// `renderPythonTree(...)` (`generator.cpp:1674-1689`).
pub fn render_python_tree(tree: &NodeTree, root_struct_id: u64) -> String {
    let idx = tree.index_of_id(root_struct_id);
    if idx < 0 {
        return String::new();
    }
    if tree.nodes[idx as usize].kind != NodeKind::Struct {
        return String::new();
    }

    let mut ctx = GenContext::new(tree, None, false);
    ctx.assign_unique_names();
    ctx.output.push_str("import ctypes\n\n");

    for sid in collect_reachable_structs(tree, &ctx.child_map, root_struct_id) {
        emit_python_struct(&mut ctx, sid);
    }
    align_comments(&ctx.output)
}

/// `renderPythonAll(...)` (`generator.cpp:1691-1706`).
pub fn render_python_all(tree: &NodeTree) -> String {
    let mut ctx = GenContext::new(tree, None, false);
    ctx.assign_unique_names();
    ctx.output.push_str("import ctypes\n\n");

    let mut roots: Vec<usize> = ctx.child_map.get(&0).cloned().unwrap_or_default();
    roots.sort_by_key(|&i| tree.nodes[i].offset);
    for ri in roots {
        if tree.nodes[ri].kind == NodeKind::Struct {
            let id = tree.nodes[ri].id;
            emit_python_struct(&mut ctx, id);
        }
    }
    align_comments(&ctx.output)
}

// ── Format dispatch (`generator.cpp:1710-1745`) ──

/// `renderCode(...)` (`generator.cpp:1710-1719`).
pub fn render_code(
    fmt: CodeFormat,
    tree: &NodeTree,
    root_struct_id: u64,
    type_aliases: Option<&TypeAliases>,
    emit_asserts: bool,
) -> String {
    match fmt {
        CodeFormat::RustStruct => render_rust(tree, root_struct_id, type_aliases, emit_asserts),
        CodeFormat::DefineOffsets => render_defines(tree, root_struct_id),
        CodeFormat::CSharpStruct => render_csharp(tree, root_struct_id, type_aliases, emit_asserts),
        CodeFormat::PythonCtypes => render_python(tree, root_struct_id),
        CodeFormat::CppHeader => render_cpp(tree, root_struct_id, type_aliases, emit_asserts),
    }
}

/// `renderCodeTree(...)` (`generator.cpp:1721-1730`).
pub fn render_code_tree(
    fmt: CodeFormat,
    tree: &NodeTree,
    root_struct_id: u64,
    type_aliases: Option<&TypeAliases>,
    emit_asserts: bool,
) -> String {
    match fmt {
        CodeFormat::RustStruct => {
            render_rust_tree(tree, root_struct_id, type_aliases, emit_asserts)
        }
        CodeFormat::DefineOffsets => render_defines_tree(tree, root_struct_id),
        CodeFormat::CSharpStruct => {
            render_csharp_tree(tree, root_struct_id, type_aliases, emit_asserts)
        }
        CodeFormat::PythonCtypes => render_python_tree(tree, root_struct_id),
        CodeFormat::CppHeader => render_cpp_tree(tree, root_struct_id, type_aliases, emit_asserts),
    }
}

/// `renderCodeAll(...)` (`generator.cpp:1732-1741`).
pub fn render_code_all(
    fmt: CodeFormat,
    tree: &NodeTree,
    type_aliases: Option<&TypeAliases>,
    emit_asserts: bool,
) -> String {
    match fmt {
        CodeFormat::RustStruct => render_rust_all(tree, type_aliases, emit_asserts),
        CodeFormat::DefineOffsets => render_defines_all(tree),
        CodeFormat::CSharpStruct => render_csharp_all(tree, type_aliases, emit_asserts),
        CodeFormat::PythonCtypes => render_python_all(tree),
        CodeFormat::CppHeader => render_cpp_all(tree, type_aliases, emit_asserts),
    }
}

/// Scope-aware dispatch: pick the right `render*` family for a [`CodeScope`].
///
/// Ports the live-view / export branch in the C++ UI (`main.cpp:5469-5477`),
/// which the Rust `generator` did not expose as a single entry point — every
/// caller (the live code pane, the export flow) re-derived the fallback rules
/// inline, so the `FullSdk → renderCodeAll` and `rootId == 0 → renderCodeAll`
/// edge cases were easy to get wrong (and the export path *did* get them
/// wrong: it passed `type_aliases = None` and ignored `emit_asserts`).
///
/// Rules, identical to the C++:
/// * [`CodeScope::FullSdk`] → [`render_code_all`] (every root struct; `root_struct_id` ignored).
/// * Otherwise, if `root_struct_id != 0`:
///   * [`CodeScope::WithChildren`] → [`render_code_tree`] (selected struct + reachable deps).
///   * [`CodeScope::Current`] → [`render_code`] (just the selected struct).
/// * Otherwise (`root_struct_id == 0`, i.e. nothing selected) → [`render_code_all`].
///
/// `type_aliases` and `emit_asserts` thread straight through to the backend,
/// so both the live view and export honor the document's alias map and the
/// persisted `generatorAsserts` option uniformly.
pub fn render_code_scoped(
    fmt: CodeFormat,
    scope: CodeScope,
    tree: &NodeTree,
    root_struct_id: u64,
    type_aliases: Option<&TypeAliases>,
    emit_asserts: bool,
) -> String {
    match scope {
        CodeScope::FullSdk => render_code_all(fmt, tree, type_aliases, emit_asserts),
        _ if root_struct_id != 0 => match scope {
            CodeScope::WithChildren => {
                render_code_tree(fmt, tree, root_struct_id, type_aliases, emit_asserts)
            }
            // `Current` (and any non-`FullSdk` scope) with a real root.
            _ => render_code(fmt, tree, root_struct_id, type_aliases, emit_asserts),
        },
        // Current / WithChildren but no struct selected ⇒ fall back to the whole SDK.
        _ => render_code_all(fmt, tree, type_aliases, emit_asserts),
    }
}

/// `renderNull(...)` (`generator.cpp:1743-1745`).
pub fn render_null(_tree: &NodeTree, _root_struct_id: u64) -> String {
    String::new()
}

#[cfg(test)]
mod tests;
