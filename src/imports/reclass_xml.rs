//! ReClass XML import + ReClassEx XML export.
//!
//! Faithful 1:1 port of `src/imports/import_reclass_xml.cpp` (392 lines) and
//! `src/imports/export_reclass_xml.cpp` (222 lines). Both PURE. Uses `quick-xml`
//! for read and write. See BMAP §2–§3.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufWriter, Read};
use std::path::Path;

use quick_xml::events::{BytesDecl, BytesText, Event};
use quick_xml::reader::Reader;
use quick_xml::writer::Writer;

use crate::core::kind::{is_hex_node, kind_to_string, size_for_kind, NodeKind};
use crate::core::node::Node;
use crate::core::tree::NodeTree;

use super::{resolve_pending_refs, ImportError, PendingRef};

// ── Version-specific type maps (cpp:14-96) ──

#[derive(Clone, Copy, PartialEq, Eq)]
enum XmlVersion {
    V2013,
    V2016,
}

/// 2016 / ReClassEx / MemeClsEx type map (`import_reclass_xml.cpp:17-52`).
const K_TYPE_MAP_2016: &[(i32, NodeKind)] = &[
    (1, NodeKind::Struct),
    (4, NodeKind::Hex32),
    (5, NodeKind::Hex64),
    (6, NodeKind::Hex16),
    (7, NodeKind::Hex8),
    (8, NodeKind::Pointer64),
    (9, NodeKind::Int64),
    (10, NodeKind::Int32),
    (11, NodeKind::Int16),
    (12, NodeKind::Int8),
    (13, NodeKind::Float),
    (14, NodeKind::Double),
    (15, NodeKind::UInt32),
    (16, NodeKind::UInt16),
    (17, NodeKind::UInt8),
    (18, NodeKind::UTF8),
    (19, NodeKind::UTF16),
    (20, NodeKind::Pointer64),
    (21, NodeKind::Hex8),
    (22, NodeKind::Vec2),
    (23, NodeKind::Vec3),
    (24, NodeKind::Vec4),
    (25, NodeKind::Mat4x4),
    (26, NodeKind::Pointer64),
    (27, NodeKind::Array),
    (29, NodeKind::Pointer64),
    (30, NodeKind::Pointer64),
    (31, NodeKind::UInt8),
    (32, NodeKind::UInt64),
    (33, NodeKind::Pointer64),
];

/// 2013 / ReClass 2011 type map (`import_reclass_xml.cpp:55-81`).
const K_TYPE_MAP_2013: &[(i32, NodeKind)] = &[
    (1, NodeKind::Struct),
    (4, NodeKind::Hex32),
    (5, NodeKind::Hex16),
    (6, NodeKind::Hex8),
    (7, NodeKind::Pointer64),
    (8, NodeKind::Int32),
    (9, NodeKind::Int16),
    (10, NodeKind::Int8),
    (11, NodeKind::Float),
    (12, NodeKind::UInt32),
    (13, NodeKind::UInt16),
    (14, NodeKind::UInt8),
    (15, NodeKind::UTF8),
    (16, NodeKind::Pointer64),
    (17, NodeKind::Hex8),
    (18, NodeKind::Vec2),
    (19, NodeKind::Vec3),
    (20, NodeKind::Vec4),
    (21, NodeKind::Mat4x4),
    (22, NodeKind::Pointer64),
    (23, NodeKind::Array),
    (27, NodeKind::Int64),
    (28, NodeKind::Double),
    (29, NodeKind::UTF16),
    (30, NodeKind::Array),
];

/// `lookupKind` (`import_reclass_xml.cpp:83-96`).
fn lookup_kind(xml_type: i32, ver: XmlVersion, ptr_size: i32) -> NodeKind {
    let table = if ver == XmlVersion::V2016 {
        K_TYPE_MAP_2016
    } else {
        K_TYPE_MAP_2013
    };
    let mut k = NodeKind::Hex8;
    for &(t, kind) in table {
        if t == xml_type {
            k = kind;
            break;
        }
    }
    // Remap pointer types for 32-bit targets
    if ptr_size < 8 && k == NodeKind::Pointer64 {
        k = NodeKind::Pointer32;
    }
    k
}

// (cpp:99-104)
fn is_pointer_type(xml_type: i32, ver: XmlVersion) -> bool {
    if ver == XmlVersion::V2016 {
        matches!(xml_type, 8 | 20 | 26 | 29 | 30 | 33)
    } else {
        matches!(xml_type, 7 | 16 | 22)
    }
}

fn is_class_instance_type(xml_type: i32, _ver: XmlVersion) -> bool {
    xml_type == 1
}

fn is_class_instance_array_type(xml_type: i32, ver: XmlVersion) -> bool {
    if ver == XmlVersion::V2016 {
        xml_type == 27
    } else {
        xml_type == 23 || xml_type == 30
    }
}

fn is_text_type(xml_type: i32, ver: XmlVersion) -> bool {
    if ver == XmlVersion::V2016 {
        xml_type == 18 || xml_type == 19
    } else {
        xml_type == 15 || xml_type == 29
    }
}

fn is_utf16_text_type(xml_type: i32, ver: XmlVersion) -> bool {
    if ver == XmlVersion::V2016 {
        xml_type == 19
    } else {
        xml_type == 29
    }
}

fn is_custom_type(xml_type: i32, ver: XmlVersion) -> bool {
    if ver == XmlVersion::V2016 {
        xml_type == 21
    } else {
        xml_type == 17
    }
}

// ── Attribute helpers (Qt toInt() → 0 on miss; toString() → "") ──

#[allow(deprecated)]
fn attr_str(e: &quick_xml::events::BytesStart, name: &str) -> String {
    match e.try_get_attribute(name.as_bytes()) {
        // `unescape_value` (no-args) is the correct decoder when the `encoding`
        // feature is off; it forwards to `normalized_value` internally and
        // matches Qt's `QXmlStreamAttribute::value()` (unescaped UTF-8).
        Ok(Some(a)) => a
            .unescape_value()
            .map(|c| c.into_owned())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn attr_int(e: &quick_xml::events::BytesStart, name: &str) -> i32 {
    // Qt toInt(): empty/missing/non-numeric → 0.
    attr_str(e, name).trim().parse::<i32>().unwrap_or(0)
}

// ── Import (cpp:142-390) ──

/// Convert a 0-based byte offset into `content` to a 1-based line number,
/// mirroring `QXmlStreamReader::lineNumber()` (the user-facing "XML parse error
/// at line N"). Counts the newlines that precede `pos`. quick-xml's
/// `buffer_position()` is a raw byte offset, so we map it back to a line here.
fn byte_pos_to_line(content: &[u8], pos: u64) -> u64 {
    let end = (pos as usize).min(content.len());
    1 + content[..end].iter().filter(|&&b| b == b'\n').count() as u64
}

pub fn import_reclass_xml(path: &Path, pointer_size: i32) -> Result<NodeTree, ImportError> {
    let mut file =
        File::open(path).map_err(|_| ImportError::CannotOpen(path.display().to_string()))?;
    // Read the whole document into memory so byte positions reported by
    // quick-xml can be mapped back to 1-based line numbers (matching the C++
    // `xml.lineNumber()` in the parse-error message).
    let mut content: Vec<u8> = Vec::new();
    file.read_to_end(&mut content)
        .map_err(|_| ImportError::CannotOpen(path.display().to_string()))?;
    let mut reader = Reader::from_reader(content.as_slice());
    reader.config_mut().trim_text(false);

    let mut version = XmlVersion::V2016; // default to 2016 (most common)

    let mut tree = NodeTree::default();
    tree.base_address = 0x0040_0000;
    tree.pointer_size = pointer_size;

    let mut class_ids: HashMap<String, u64> = HashMap::new();
    let mut pending_refs: Vec<PendingRef> = Vec::new();

    let mut version_detected = false;

    let mut buf: Vec<u8> = Vec::new();
    // State for the currently-open Class element.
    let mut in_class = false;
    let mut struct_id: u64 = 0;
    let mut child_offset: i32 = 0;

    loop {
        let ev = match reader.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(e) => {
                // Genuine malformed XML mid-stream → error. (Qt tolerates
                // PrematureEndOfDocumentError; quick-xml returns Ok(Eof) on a
                // clean end, so any Err here is a real parse error.)
                return Err(ImportError::XmlParse {
                    line: byte_pos_to_line(&content, reader.buffer_position()),
                    msg: e.to_string(),
                });
            }
        };

        match ev {
            Event::Eof => break,
            Event::Comment(t) => {
                if !version_detected {
                    let comment = t.decode().map(|c| c.into_owned()).unwrap_or_default();
                    let comment = comment.trim();
                    let cl = comment.to_ascii_lowercase();
                    if cl.contains("reclassex")
                        || cl.contains("memeclsex")
                        || cl.contains("2016")
                        || cl.contains("2015")
                    {
                        version = XmlVersion::V2016;
                    } else if cl.contains("2013") || cl.contains("2011") {
                        version = XmlVersion::V2013;
                    }
                    version_detected = true;
                }
            }
            ev @ (Event::Start(_) | Event::Empty(_)) => {
                // `is_empty` distinguishes a self-closing `<Class …/>` (delivered
                // by quick-xml as `Event::Empty`, with no matching `Event::End`)
                // from an opening `<Class …>`. QXmlStreamReader reports a
                // self-closing element as a StartElement immediately followed by
                // an EndElement, so a childless `<Class …/>` must NOT leave
                // `in_class` set — otherwise every later `<Class>` is dropped.
                let is_empty = matches!(ev, Event::Empty(_));
                let e = match ev {
                    Event::Start(e) => e,
                    Event::Empty(e) => e,
                    _ => unreachable!(),
                };
                let name = e.name();
                let local = name.as_ref();
                if !in_class {
                    if local == b"Class" {
                        let class_name = attr_str(&e, "Name");
                        let _str_offset = attr_str(&e, "strOffset"); // read but unused
                        let root = Node {
                            kind: NodeKind::Struct,
                            name: class_name.clone(),
                            struct_type_name: class_name.clone(),
                            parent_id: 0,
                            offset: 0,
                            collapsed: true,
                            ..Node::default()
                        };
                        let idx = tree.add_node(root);
                        struct_id = tree.nodes[idx].id;
                        class_ids.insert(class_name, struct_id);
                        child_offset = 0;
                        // A self-closing/childless `<Class …/>` is immediately
                        // closed; only an opening `<Class …>` keeps us in-class.
                        in_class = !is_empty;
                    }
                    // else: ignore (ReClass / decl etc.)
                } else if local == b"Node" {
                    // Extract all attributes into owned values BEFORE borrowing
                    // the reader for any inner reads (ClassInstanceArray).
                    let attrs = NodeAttrs {
                        xml_type: attr_int(&e, "Type"),
                        node_name: attr_str(&e, "Name"),
                        node_size: attr_int(&e, "Size"),
                        ptr_class: attr_str(&e, "Pointer"),
                        inst_class: attr_str(&e, "Instance"),
                        total: attr_int(&e, "Total"),
                        count: attr_int(&e, "Count"),
                    };
                    handle_node(
                        &mut reader,
                        &content,
                        attrs,
                        is_empty,
                        version,
                        pointer_size,
                        struct_id,
                        &mut child_offset,
                        &mut tree,
                        &mut pending_refs,
                    )?;
                }
            }
            Event::End(e) => {
                if in_class && e.name().as_ref() == b"Class" {
                    in_class = false;
                }
            }
            _ => {}
        }
        buf.clear();
    }

    if tree.nodes.is_empty() {
        return Err(ImportError::NoClasses);
    }

    resolve_pending_refs(&mut tree, &pending_refs, &class_ids);
    Ok(tree)
}

/// Owned attribute snapshot of a `<Node>` element.
struct NodeAttrs {
    xml_type: i32,
    node_name: String,
    node_size: i32,
    ptr_class: String,
    inst_class: String,
    total: i32,
    count: i32,
}

#[allow(clippy::too_many_arguments)]
fn handle_node<B: BufRead>(
    reader: &mut Reader<B>,
    content: &[u8],
    attrs: NodeAttrs,
    is_empty: bool,
    version: XmlVersion,
    pointer_size: i32,
    struct_id: u64,
    child_offset: &mut i32,
    tree: &mut NodeTree,
    pending_refs: &mut Vec<PendingRef>,
) -> Result<(), ImportError> {
    let NodeAttrs {
        xml_type,
        node_name,
        node_size,
        ptr_class,
        inst_class,
        total: node_total,
        count: node_count,
    } = attrs;
    let mut buf: Vec<u8> = Vec::new();
    let buf = &mut buf;

    // (a) Custom type: expand to appropriate hex nodes (cpp:231-255)
    if is_custom_type(xml_type, version) && node_size > 0 {
        let (hex_kind, hex_size) = if node_size >= 8 && node_size % 8 == 0 {
            (NodeKind::Hex64, 8)
        } else if node_size >= 4 && node_size % 4 == 0 {
            (NodeKind::Hex32, 4)
        } else if node_size >= 2 && node_size % 2 == 0 {
            (NodeKind::Hex16, 2)
        } else {
            (NodeKind::Hex8, 1)
        };
        let count = node_size / hex_size;
        for _ in 0..count {
            let n = Node {
                kind: hex_kind,
                name: if count == 1 {
                    node_name.clone()
                } else {
                    String::new()
                },
                parent_id: struct_id,
                offset: *child_offset,
                ..Node::default()
            };
            tree.add_node(n);
            *child_offset += hex_size;
        }
        return Ok(());
    }

    let kind = lookup_kind(xml_type, version, pointer_size);

    // (c) ClassInstanceArray (cpp:260-302)
    if is_class_instance_array_type(xml_type, version) {
        let mut total = node_total;
        if total <= 0 {
            total = node_count;
        }
        if total <= 0 {
            total = 1;
        }

        // Read child <Array> element for class name. Iterate inner events until
        // </Node> (cpp:269-280).
        //
        // A self-closing `<Node Type="27" .../>` has no children. QXmlStreamReader
        // reports it as a StartElement immediately followed by an EndElement, so
        // the C++ inner loop's first `readNext()` yields the `</Node>` end and
        // breaks at once. quick-xml delivers a self-closing element as a single
        // `Event::Empty` with NO matching `Event::End`, so running the inner loop
        // would read past this node into following siblings (or to EOF). Skip the
        // inner loop entirely when the element is self-closing.
        let mut array_class_name = String::new();
        if !is_empty {
            loop {
                let inner = match reader.read_event_into(buf) {
                    Ok(ev) => ev,
                    Err(err) => {
                        return Err(ImportError::XmlParse {
                            line: byte_pos_to_line(content, reader.buffer_position()),
                            msg: err.to_string(),
                        })
                    }
                };
                match inner {
                    Event::Eof => break,
                    Event::End(ee) if ee.name().as_ref() == b"Node" => break,
                    Event::Start(ae) | Event::Empty(ae) if ae.name().as_ref() == b"Array" => {
                        array_class_name = attr_str(&ae, "Name");
                        let mut array_total = attr_int(&ae, "Total");
                        if array_total <= 0 {
                            array_total = attr_int(&ae, "Count");
                        }
                        if array_total > 0 {
                            total = array_total;
                        }
                    }
                    _ => {}
                }
            }
        }

        let mut arr = Node {
            kind: NodeKind::Array,
            name: node_name,
            parent_id: struct_id,
            offset: *child_offset,
            array_len: total,
            element_kind: NodeKind::Struct,
            ..Node::default()
        };
        if !array_class_name.is_empty() {
            arr.struct_type_name = array_class_name.clone();
        }
        let arr_idx = tree.add_node(arr);
        let arr_id = tree.nodes[arr_idx].id;

        if !array_class_name.is_empty() {
            pending_refs.push(PendingRef {
                node_id: arr_id,
                class_name: array_class_name,
            });
        }

        *child_offset += if node_size > 0 { node_size } else { 0 };
        return Ok(());
    }

    // (d) Build a Node
    let mut n = Node {
        kind,
        name: node_name,
        parent_id: struct_id,
        offset: *child_offset,
        ..Node::default()
    };

    // Text nodes (cpp:311-316)
    if is_text_type(xml_type, version) {
        if is_utf16_text_type(xml_type, version) {
            n.str_len = (node_size / 2).max(1);
        } else {
            n.str_len = node_size.max(1);
        }
    }

    // Pointer types (cpp:319-327)
    if is_pointer_type(xml_type, version) && !ptr_class.is_empty() {
        n.collapsed = true;
        let node_idx = tree.add_node(n);
        let node_id = tree.nodes[node_idx].id;
        pending_refs.push(PendingRef {
            node_id,
            class_name: ptr_class,
        });
        *child_offset += if node_size > 0 {
            node_size
        } else {
            size_for_kind(kind)
        };
        return Ok(());
    }

    // Embedded class instance (cpp:330-344)
    if is_class_instance_type(xml_type, version) {
        let resolved_class = if inst_class.is_empty() {
            ptr_class
        } else {
            inst_class
        };
        n.collapsed = true;
        n.struct_type_name = resolved_class;
        if !n.struct_type_name.is_empty() {
            let class_name = n.struct_type_name.clone();
            let node_idx = tree.add_node(n);
            let node_id = tree.nodes[node_idx].id;
            pending_refs.push(PendingRef {
                node_id,
                class_name,
            });
        } else {
            tree.add_node(n);
        }
        *child_offset += if node_size > 0 { node_size } else { 0 };
        return Ok(());
    }

    tree.add_node(n);
    *child_offset += if node_size > 0 {
        node_size
    } else {
        size_for_kind(kind)
    };
    Ok(())
}

// ── Export (cpp:11-220) ──

/// `xmlTypeForKind` (`export_reclass_xml.cpp:11-40`) — reverse of the 2016 map.
fn xml_type_for_kind(kind: NodeKind) -> i32 {
    match kind {
        NodeKind::Struct => 1,
        NodeKind::Hex32 => 4,
        NodeKind::Hex64 => 5,
        NodeKind::Hex16 => 6,
        NodeKind::Hex8 => 7,
        NodeKind::Pointer64 => 8,
        NodeKind::Pointer32 => 8,
        NodeKind::Int64 => 9,
        NodeKind::Int32 => 10,
        NodeKind::Int16 => 11,
        NodeKind::Int8 => 12,
        NodeKind::Float => 13,
        NodeKind::Double => 14,
        NodeKind::UInt32 => 15,
        NodeKind::UInt16 => 16,
        NodeKind::UInt8 => 17,
        NodeKind::UInt64 => 32,
        NodeKind::UTF8 => 18,
        NodeKind::UTF16 => 19,
        NodeKind::Bool => 17, // No native bool in ReClass, map to UInt8
        NodeKind::Vec2 => 22,
        NodeKind::Vec3 => 23,
        NodeKind::Vec4 => 24,
        NodeKind::Mat4x4 => 25,
        NodeKind::Array => 27,
        _ => 7, // fallback to Hex8
    }
}

/// `nodeSizeForExport` (`export_reclass_xml.cpp:42-52`).
fn node_size_for_export(node: &Node) -> i32 {
    match node.kind {
        NodeKind::UTF8 => node.str_len,
        NodeKind::UTF16 => node.str_len * 2,
        NodeKind::Array => {
            let elem_sz = size_for_kind(node.element_kind);
            node.array_len * if elem_sz > 0 { elem_sz } else { 0 }
        }
        _ => size_for_kind(node.kind),
    }
}

/// `resolveStructName` (`export_reclass_xml.cpp:55-61`).
fn resolve_struct_name(tree: &NodeTree, ref_id: u64) -> String {
    let idx = tree.index_of_id(ref_id);
    if idx < 0 {
        return String::new();
    }
    let r = &tree.nodes[idx as usize];
    if !r.struct_type_name.is_empty() {
        r.struct_type_name.clone()
    } else {
        r.name.clone()
    }
}

pub fn export_reclass_xml(tree: &NodeTree, path: &Path) -> Result<(), ImportError> {
    if tree.nodes.is_empty() {
        return Err(ImportError::NoNodesToExport);
    }

    let file =
        File::create(path).map_err(|_| ImportError::CannotOpenWrite(path.display().to_string()))?;

    // Build child map (cpp:76-78)
    let mut child_map: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, n) in tree.nodes.iter().enumerate() {
        child_map.entry(n.parent_id).or_default().push(i);
    }

    let mut writer = Writer::new_with_indent(BufWriter::new(file), b' ', 4);
    // C++ `QXmlStreamWriter::writeStartDocument()` emits
    // `<?xml version="1.0" encoding="UTF-8"?>` with NO standalone attribute.
    // Passing `None` for the standalone argument matches byte-for-byte.
    writer
        .write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
        .map_err(|e| ImportError::Io(e.to_string()))?;

    // <ReClass>
    writer
        .write_event(Event::Start(quick_xml::events::BytesStart::new("ReClass")))
        .map_err(|e| ImportError::Io(e.to_string()))?;
    // <!--ReClassEx--> (importer version-detection key)
    writer
        .write_event(Event::Comment(BytesText::new("ReClassEx")))
        .map_err(|e| ImportError::Io(e.to_string()))?;

    // Roots sorted by offset (cpp:89-92)
    let mut roots = child_map.get(&0).cloned().unwrap_or_default();
    roots.sort_by_key(|&a| tree.nodes[a].offset);

    let mut class_count = 0;

    for ri in roots {
        let root = &tree.nodes[ri];
        if root.kind != NodeKind::Struct {
            continue;
        }

        // <Class ...> attributes in this exact order (cpp:100-106)
        let class_name = if root.name.is_empty() {
            root.struct_type_name.clone()
        } else {
            root.name.clone()
        };
        let class_attrs: [(&str, &str); 6] = [
            ("Name", class_name.as_str()),
            ("Type", "28"),
            ("Comment", ""),
            ("Offset", "0"),
            ("strOffset", "0"),
            ("Code", ""),
        ];
        writer
            .write_event(Event::Start(
                quick_xml::events::BytesStart::new("Class")
                    .with_attributes(class_attrs.iter().copied()),
            ))
            .map_err(|e| ImportError::Io(e.to_string()))?;

        // Children sorted by offset (cpp:109-112)
        let mut children = child_map.get(&root.id).cloned().unwrap_or_default();
        children.sort_by_key(|&a| tree.nodes[a].offset);

        let mut i = 0usize;
        while i < children.len() {
            let child = &tree.nodes[children[i]];

            // (A) Bitfield container (cpp:118-134)
            if child.kind == NodeKind::Struct && child.resolved_class_keyword() == "bitfield" {
                let mut sz = child.byte_size();
                if sz <= 0 {
                    sz = 4;
                }
                let hex_kind = if sz <= 1 {
                    NodeKind::Hex8
                } else if sz <= 2 {
                    NodeKind::Hex16
                } else if sz <= 4 {
                    NodeKind::Hex32
                } else {
                    NodeKind::Hex64
                };
                let type_str = xml_type_for_kind(hex_kind).to_string();
                let size_str = sz.to_string();
                let attrs: [(&str, &str); 5] = [
                    ("Name", child.name.as_str()),
                    ("Type", type_str.as_str()),
                    ("Size", size_str.as_str()),
                    ("bHidden", "false"),
                    ("Comment", "bitfield"),
                ];
                writer
                    .write_event(Event::Empty(
                        quick_xml::events::BytesStart::new("Node")
                            .with_attributes(attrs.iter().copied()),
                    ))
                    .map_err(|e| ImportError::Io(e.to_string()))?;
                i += 1;
                continue;
            }

            // (B) Hex-run collapse (cpp:137-160)
            if is_hex_node(child.kind) {
                let run_start = child.offset;
                let mut run_end = child.offset + child.byte_size();
                let mut j = i + 1;
                while j < children.len() {
                    let next = &tree.nodes[children[j]];
                    if !is_hex_node(next.kind) {
                        break;
                    }
                    if next.offset < run_end {
                        break; // overlap/gap stops the run
                    }
                    run_end = next.offset + next.byte_size();
                    j += 1;
                }
                let total_size = run_end - run_start;
                let hex_name = if j - i == 1 && !child.name.is_empty() {
                    child.name.clone()
                } else {
                    String::new()
                };
                let size_str = total_size.to_string();
                let attrs: [(&str, &str); 5] = [
                    ("Name", hex_name.as_str()),
                    ("Type", "21"),
                    ("Size", size_str.as_str()),
                    ("bHidden", "false"),
                    ("Comment", ""),
                ];
                writer
                    .write_event(Event::Empty(
                        quick_xml::events::BytesStart::new("Node")
                            .with_attributes(attrs.iter().copied()),
                    ))
                    .map_err(|e| ImportError::Io(e.to_string()))?;
                i = j;
                continue;
            }

            // (C) Generic node (cpp:162-203) — Start + (optional Array) + End
            let type_str = xml_type_for_kind(child.kind).to_string();
            let size_str = node_size_for_export(child).to_string();
            let mut attrs: Vec<(&str, String)> = vec![
                ("Name", child.name.clone()),
                ("Type", type_str),
                ("Size", size_str),
                ("bHidden", "false".to_string()),
                ("Comment", String::new()),
            ];

            // Pointer with target
            if (child.kind == NodeKind::Pointer64 || child.kind == NodeKind::Pointer32)
                && child.ref_id != 0
            {
                let target = resolve_struct_name(tree, child.ref_id);
                if !target.is_empty() {
                    attrs.push(("Pointer", target));
                }
            }

            // Embedded struct instance
            if child.kind == NodeKind::Struct {
                let inst_name = if child.struct_type_name.is_empty() {
                    child.name.clone()
                } else {
                    child.struct_type_name.clone()
                };
                attrs.push(("Instance", inst_name));
            }

            // Array: Total + child <Array>
            let mut array_elem_name: Option<String> = None;
            if child.kind == NodeKind::Array {
                attrs.push(("Total", child.array_len.to_string()));

                let mut elem_name = if child.element_kind == NodeKind::Struct
                    && !child.struct_type_name.is_empty()
                {
                    child.struct_type_name.clone()
                } else if child.ref_id != 0 {
                    resolve_struct_name(tree, child.ref_id)
                } else {
                    String::new()
                };
                if elem_name.is_empty() {
                    elem_name = kind_to_string(child.element_kind).to_string();
                }
                array_elem_name = Some(elem_name);
            }

            let node_start = quick_xml::events::BytesStart::new("Node")
                .with_attributes(attrs.iter().map(|(k, v)| (*k, v.as_str())));
            match array_elem_name {
                // Array node: has a child <Array> element, so the <Node> stays
                // open (Start + child + End), matching QXmlStreamWriter.
                Some(elem_name) => {
                    writer
                        .write_event(Event::Start(node_start))
                        .map_err(|e| ImportError::Io(e.to_string()))?;

                    let total_str = child.array_len.to_string();
                    let arr_attrs: [(&str, &str); 2] =
                        [("Name", elem_name.as_str()), ("Total", total_str.as_str())];
                    writer
                        .write_event(Event::Empty(
                            quick_xml::events::BytesStart::new("Array")
                                .with_attributes(arr_attrs.iter().copied()),
                        ))
                        .map_err(|e| ImportError::Io(e.to_string()))?;

                    writer
                        .write_event(Event::End(quick_xml::events::BytesEnd::new("Node")))
                        .map_err(|e| ImportError::Io(e.to_string()))?;
                }
                // Childless generic node (pointer / primitive / struct instance):
                // QXmlStreamWriter with auto-formatting collapses an element with
                // no children to a self-closing `<Node …/>`. Emit a single
                // Event::Empty to match byte-for-byte.
                None => {
                    writer
                        .write_event(Event::Empty(node_start))
                        .map_err(|e| ImportError::Io(e.to_string()))?;
                }
            }

            i += 1;
        }

        writer
            .write_event(Event::End(quick_xml::events::BytesEnd::new("Class")))
            .map_err(|e| ImportError::Io(e.to_string()))?;
        class_count += 1;
    }

    writer
        .write_event(Event::End(quick_xml::events::BytesEnd::new("ReClass")))
        .map_err(|e| ImportError::Io(e.to_string()))?;

    // flush
    writer
        .into_inner()
        .into_inner()
        .map_err(|e| ImportError::Io(e.to_string()))?;

    if class_count == 0 {
        return Err(ImportError::NoClassesToExport);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("rcx_xml_unit_{}_{}.reclass", tag, nanos));
        p
    }

    fn import_str(xml: &str) -> Result<NodeTree, ImportError> {
        let path = tmp("import");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(xml.as_bytes())
            .unwrap();
        let r = import_reclass_xml(&path, 8);
        let _ = std::fs::remove_file(&path);
        r
    }

    fn export_str(tree: &NodeTree) -> String {
        let path = tmp("export");
        export_reclass_xml(tree, &path).expect("export should succeed");
        let s = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        s
    }

    // ── Item 6: byte position → 1-based line ──

    #[test]
    fn byte_pos_to_line_counts_newlines() {
        //               0123 4 567890 1 2345 6
        //               <a>\n  <b/>\n </a>\n
        let content = b"<a>\n  <b/>\n</a>\n";
        assert_eq!(byte_pos_to_line(content, 0), 1);
        assert_eq!(byte_pos_to_line(content, 3), 1); // at the first '\n' (not yet counted)
        assert_eq!(byte_pos_to_line(content, 4), 2); // just after first '\n'
        assert_eq!(byte_pos_to_line(content, 11), 3); // after the 2nd '\n'
                                                      // The content ends with a trailing '\n' (3 newlines total), so any
                                                      // position at/after it is line 4; out-of-range clamps to len.
        assert_eq!(byte_pos_to_line(content, 9999), 4);
    }

    #[test]
    fn xml_parse_error_reports_line_not_byte_offset() {
        // Malformed close tag on line 3 — the error must carry a small 1-based
        // line number, not the (much larger) raw byte offset.
        let xml = "<ReClass>\n  <Class Name=\"A\">\n    <Node Type=\"10\" Size=\"4\"</Class>\n</ReClass>\n";
        match import_str(xml) {
            Err(ImportError::XmlParse { line, .. }) => {
                assert!(
                    line <= 4,
                    "expected a 1-based line number (<=4), got {line}"
                );
            }
            other => panic!("expected XmlParse error, got {other:?}"),
        }
    }

    // ── Item 3: self-closing empty <Class/> must not stall the parser ──

    #[test]
    fn empty_self_closing_class_does_not_drop_later_classes() {
        let xml = "\
<ReClass>
  <Class Name=\"Empty\"/>
  <Class Name=\"After\">
    <Node Type=\"10\" Name=\"x\" Size=\"4\"/>
  </Class>
</ReClass>
";
        let tree = import_str(xml).expect("import should succeed");
        let names: Vec<&str> = tree.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"Empty"),
            "empty class must still be imported, got {names:?}"
        );
        assert!(
            names.contains(&"After"),
            "class following an empty self-closing <Class/> must NOT be dropped, got {names:?}"
        );
        // The "After" class must have its child node attached.
        let after_idx = tree.nodes.iter().position(|n| n.name == "After").unwrap();
        let after_id = tree.nodes[after_idx].id;
        assert_eq!(tree.children_of(after_id).len(), 1);
    }

    #[test]
    fn empty_class_with_explicit_close_still_works() {
        // Sanity: the non-self-closing empty form continues to behave.
        let xml = "\
<ReClass>
  <Class Name=\"Empty\"></Class>
  <Class Name=\"After\">
    <Node Type=\"10\" Name=\"x\" Size=\"4\"/>
  </Class>
</ReClass>
";
        let tree = import_str(xml).expect("import");
        let names: Vec<&str> = tree.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"Empty") && names.contains(&"After"));
    }

    // ── Self-closing ClassInstanceArray <Node/> must not over-consume ──

    #[test]
    fn self_closing_class_instance_array_node_does_not_over_consume() {
        // A self-closing `<Node Type="27" .../>` (ClassInstanceArray, no inner
        // <Array> child) is followed by a sibling primitive node, then a second
        // class. quick-xml delivers the CIA node as Event::Empty with no matching
        // </Node>; the importer must NOT run the inner read loop, or it would
        // walk over the sibling node and the following </Class>/<Class>.
        let xml = "\
<ReClass>
  <Class Name=\"A\">
    <Node Type=\"27\" Name=\"arr\" Total=\"3\" Size=\"0\"/>
    <Node Type=\"10\" Name=\"after\" Size=\"4\"/>
  </Class>
  <Class Name=\"B\">
    <Node Type=\"10\" Name=\"b0\" Size=\"4\"/>
  </Class>
</ReClass>
";
        let tree = import_str(xml).expect("import should succeed");

        // Both top-level classes must survive (the CIA node must not eat the
        // following sibling/class boundaries).
        let class_names: Vec<&str> = tree
            .nodes
            .iter()
            .filter(|n| n.parent_id == 0 && n.kind == NodeKind::Struct)
            .map(|n| n.name.as_str())
            .collect();
        assert!(
            class_names.contains(&"A") && class_names.contains(&"B"),
            "both classes must import, got {class_names:?}"
        );

        // Class A must keep BOTH children: the CIA array AND the sibling that
        // follows it on the next line.
        let a_idx = tree
            .nodes
            .iter()
            .position(|n| n.parent_id == 0 && n.name == "A")
            .unwrap();
        let a_id = tree.nodes[a_idx].id;
        let a_children = tree.children_of(a_id);
        let child_names: Vec<&str> = a_children
            .iter()
            .map(|&ci| tree.nodes[ci].name.as_str())
            .collect();
        assert!(
            child_names.contains(&"arr"),
            "CIA array node must be imported, got {child_names:?}"
        );
        assert!(
            child_names.contains(&"after"),
            "the sibling node after a self-closing CIA node must NOT be consumed, got {child_names:?}"
        );

        // Class B's child must remain attached to B (not stolen by A's CIA loop).
        let b_idx = tree
            .nodes
            .iter()
            .position(|n| n.parent_id == 0 && n.name == "B")
            .unwrap();
        let b_id = tree.nodes[b_idx].id;
        assert_eq!(
            tree.children_of(b_id).len(),
            1,
            "class B must keep its own child"
        );
    }

    #[test]
    fn open_class_instance_array_node_still_reads_inner_array() {
        // Sanity: a non-self-closing CIA node with an inner <Array> child still
        // resolves the element class name and array length via the inner loop.
        let xml = "\
<ReClass>
  <Class Name=\"Elem\">
    <Node Type=\"10\" Name=\"e0\" Size=\"4\"/>
  </Class>
  <Class Name=\"Host\">
    <Node Type=\"27\" Name=\"arr\" Total=\"2\" Size=\"8\">
      <Array Name=\"Elem\" Total=\"5\"/>
    </Node>
  </Class>
</ReClass>
";
        let tree = import_str(xml).expect("import should succeed");
        let host_idx = tree
            .nodes
            .iter()
            .position(|n| n.parent_id == 0 && n.name == "Host")
            .unwrap();
        let host_id = tree.nodes[host_idx].id;
        let arr_ci = *tree.children_of(host_id).first().unwrap();
        let arr = &tree.nodes[arr_ci];
        assert_eq!(arr.kind, NodeKind::Array);
        assert_eq!(arr.name, "arr");
        // Inner <Array Total="5"> overrides the node-level Total="2".
        assert_eq!(arr.array_len, 5);
        assert_eq!(arr.struct_type_name, "Elem");
    }

    // ── Item 4: XML declaration has no standalone attribute ──

    #[test]
    fn export_declaration_has_no_standalone() {
        let mut tree = NodeTree::default();
        tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "S".to_string(),
            struct_type_name: "S".to_string(),
            ..Node::default()
        });
        let s = export_str(&tree);
        let first_line = s.lines().next().unwrap_or("");
        assert_eq!(
            first_line.trim(),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
            "declaration must match QXmlStreamWriter (no standalone)"
        );
        assert!(!s.contains("standalone"));
    }

    // ── Item 5: childless generic <Node> is self-closing ──

    #[test]
    fn export_generic_node_is_self_closing() {
        let mut tree = NodeTree::default();
        let root = tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "S".to_string(),
            struct_type_name: "S".to_string(),
            ..Node::default()
        });
        let root_id = tree.nodes[root].id;
        // A primitive (UInt32) child — no inner <Array> element.
        tree.add_node(Node {
            kind: NodeKind::UInt32,
            name: "field".to_string(),
            parent_id: root_id,
            offset: 0,
            ..Node::default()
        });
        let s = export_str(&tree);
        // The Node must self-close, never as <Node ...></Node>.
        assert!(
            s.contains("<Node ") && s.contains("/>"),
            "generic node should be present and self-closing:\n{s}"
        );
        assert!(
            !s.contains("</Node>"),
            "childless generic node must self-close, not Start+End:\n{s}"
        );
    }

    #[test]
    fn export_array_node_stays_open_with_array_child() {
        // An Array node DOES have a child <Array> element, so it must remain
        // Start + child + End (matching QXmlStreamWriter).
        let mut tree = NodeTree::default();
        let root = tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "S".to_string(),
            struct_type_name: "S".to_string(),
            ..Node::default()
        });
        let root_id = tree.nodes[root].id;
        tree.add_node(Node {
            kind: NodeKind::Array,
            name: "arr".to_string(),
            parent_id: root_id,
            offset: 0,
            array_len: 4,
            element_kind: NodeKind::Int32,
            ..Node::default()
        });
        let s = export_str(&tree);
        assert!(
            s.contains("</Node>"),
            "array node keeps an explicit close:\n{s}"
        );
        assert!(
            s.contains("<Array "),
            "array node emits a child <Array>:\n{s}"
        );
    }
}
