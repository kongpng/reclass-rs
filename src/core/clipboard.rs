//! Clipboard codec — serialize a node selection (plus subtrees) to a portable
//! JSON blob + plain-text listing, and deserialize with freshly minted ids.
//!
//! Faithful port of `src/clipboard.h` (242 lines). The C++ produces a
//! `QMimeData` carrying both the custom MIME-typed JSON and a plain-text
//! fallback; the Rust port returns the two payloads directly (the actual
//! clipboard handoff is the UI layer's job — see `ui::`). The custom MIME type
//! string is preserved verbatim for interop.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use super::kind::kind_to_string;
use super::node::Node;
use super::tree::NodeTree;

/// `ClipboardCodec::kMimeType` (`clipboard.h:33`).
pub const K_MIME_TYPE: &str = "application/x-reclass-nodes-v1";

/// The schema tag written into the JSON payload (`clipboard.h:79`).
pub const K_SCHEMA: &str = "rcx-clipboard/v1";

/// `ClipboardCodec::PasteResult` (`clipboard.h:93-96`).
#[derive(Clone, Debug, Default)]
pub struct PasteResult {
    /// ready to insert (ids remapped to non-colliding fresh ids).
    pub nodes: Vec<Node>,
    /// new ids corresponding to the original roots.
    pub root_ids: Vec<u64>,
}

/// `ClipboardCodec::collectSubtrees` (`clipboard.h:37-54`) — node + all
/// descendants, iterative & cycle-safe.
pub fn collect_subtrees(tree: &NodeTree, roots: &[u64]) -> Vec<Node> {
    let mut out = Vec::new();
    let mut seen: HashSet<u64> = HashSet::new();
    let mut stack: Vec<u64> = roots.to_vec();
    while let Some(id) = stack.pop() {
        if seen.contains(&id) {
            continue;
        }
        seen.insert(id);
        let idx = tree.index_of_id(id);
        if idx < 0 {
            continue;
        }
        out.push(tree.nodes[idx as usize].clone());
        for ci in tree.children_of(id) {
            stack.push(tree.nodes[ci].id);
        }
    }
    out
}

/// `ClipboardCodec::serialize` (`clipboard.h:58-88`). Returns
/// `(json_payload_bytes, plain_text)` — the two clipboard representations.
pub fn serialize(
    tree: &NodeTree,
    root_ids: &[u64],
    clear_parent_for: &HashSet<u64>,
) -> (Vec<u8>, String) {
    let nodes = collect_subtrees(tree, root_ids);
    let root_arr: Vec<Value> = root_ids.iter().map(|r| json!(r.to_string())).collect();
    let node_arr: Vec<Value> = nodes
        .iter()
        .map(|n| {
            let mut o = n.to_json();
            if clear_parent_for.contains(&n.id) {
                o["parentId"] = json!("0");
            }
            o
        })
        .collect();
    let payload = json!({
        "schema": K_SCHEMA,
        "roots": Value::Array(root_arr),
        "nodes": Value::Array(node_arr),
    });
    let bytes = serde_json::to_vec(&payload).unwrap_or_default();
    (bytes, plain_dump(tree, root_ids))
}

/// `ClipboardCodec::deserialize` (`clipboard.h:97-127`). Parses the custom MIME
/// blob, remaps ids to non-colliding fresh ids via `tree.reserve_id()`, and
/// rewires parent/refId references.
pub fn deserialize(tree: &mut NodeTree, blob: &[u8]) -> PasteResult {
    let mut r = PasteResult::default();
    let doc: Value = match serde_json::from_slice(blob) {
        Ok(v) => v,
        Err(_) => return r,
    };
    if !doc.is_object() {
        return r;
    }
    if doc.get("schema").and_then(Value::as_str) != Some(K_SCHEMA) {
        return r;
    }
    let raw: Vec<Node> = doc
        .get("nodes")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(Node::from_json).collect())
        .unwrap_or_default();
    if raw.is_empty() {
        return r;
    }
    // Build old → new id map.
    let mut id_map: HashMap<u64, u64> = HashMap::new();
    for n in &raw {
        id_map.insert(n.id, tree.reserve_id());
    }
    // Rewire.
    r.nodes.reserve(raw.len());
    for mut n in raw {
        n.id = *id_map.get(&n.id).unwrap_or(&n.id);
        n.parent_id = *id_map.get(&n.parent_id).unwrap_or(&n.parent_id);
        n.ref_id = *id_map.get(&n.ref_id).unwrap_or(&n.ref_id);
        r.nodes.push(n);
    }
    if let Some(arr) = doc.get("roots").and_then(Value::as_array) {
        for v in arr {
            let old = v.as_str().unwrap_or("0").trim().parse::<u64>().unwrap_or(0);
            r.root_ids.push(*id_map.get(&old).unwrap_or(&0));
        }
    }
    r
}

/// `ClipboardCodec::parseLenientHex` (`clipboard.h:148-209`). Tokenize on any
/// non-hex character, treat each token as a hex number, left-pad odd tokens to
/// an even nibble count, and concatenate. Returns `Err(reason)` on a malformed
/// token (or empty input with no hex digits).
pub fn parse_lenient_hex(src: &str) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::with_capacity(src.len() / 2);
    let mut tok = String::new();
    let is_hex = |c: char| c.is_ascii_hexdigit();

    fn flush(tok: &mut String, out: &mut Vec<u8>) -> Result<(), String> {
        if tok.is_empty() {
            return Ok(());
        }
        // Strip optional 0x/0X prefix on the token (not mid-token).
        if tok.len() > 2
            && tok.starts_with('0')
            && (tok.as_bytes()[1] == b'x' || tok.as_bytes()[1] == b'X')
        {
            tok.drain(0..2);
        }
        for c in tok.chars() {
            if !c.is_ascii_hexdigit() {
                return Err(format!("Invalid hex digit '{c}'"));
            }
        }
        if tok.len() & 1 == 1 {
            tok.insert(0, '0');
        }
        let bytes = tok.as_bytes();
        let mut i = 0;
        while i + 1 < bytes.len() {
            let hi = (bytes[i] as char).to_digit(16).ok_or("Parse failed")?;
            let lo = (bytes[i + 1] as char).to_digit(16).ok_or("Parse failed")?;
            out.push(((hi << 4) | lo) as u8);
            i += 2;
        }
        tok.clear();
        Ok(())
    }

    for c in src.chars() {
        if is_hex(c) {
            tok.push(c);
            continue;
        }
        // "0x" stays in the token to be stripped by flush().
        if (c == 'x' || c == 'X') && tok == "0" {
            tok.push(c);
            continue;
        }
        // Stray letter (incl. a mid-token x/X) is malformed.
        if c.is_alphabetic() {
            return Err(format!("Invalid hex digit '{c}'"));
        }
        flush(&mut tok, &mut out)?;
    }
    flush(&mut tok, &mut out)?;
    if out.is_empty() {
        return Err("No hex data".into());
    }
    Ok(out)
}

/// `ClipboardCodec::plainDump` (`clipboard.h:212-222`) — human-readable listing.
pub fn plain_dump(tree: &NodeTree, root_ids: &[u64]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for &r in root_ids {
        let idx = tree.index_of_id(r);
        if idx < 0 {
            continue;
        }
        dump_node(tree, idx as usize, 0, &mut lines);
    }
    lines.join("\n")
}

/// `ClipboardCodec::dumpNode` (`clipboard.h:225-238`).
fn dump_node(tree: &NodeTree, idx: usize, depth: i32, out: &mut Vec<String>) {
    let n = &tree.nodes[idx];
    let indent = " ".repeat((depth * 2) as usize);
    let kind_name = kind_to_string(n.kind);
    // Qt's `QString::arg(offset, 4, 16, '0')` renders the offset as
    // sign-magnitude: a negative value emits a leading '-' and zero-pads the
    // magnitude within the width-4 field (the sign occupies one slot), e.g.
    // `-001` for -1, `-0ff` for -255. Rust's `{:04x}` on an i32 would instead
    // print the two's-complement pattern (`ffffffff`), so format the magnitude
    // explicitly and reserve a field slot for the sign.
    let sign = if n.offset < 0 { "-" } else { "" };
    let width = 4usize.saturating_sub(sign.len());
    out.push(format!(
        "{indent}+0x{sign}{:0width$x}  {:<8}  {}",
        n.offset.unsigned_abs(),
        kind_name,
        n.name,
        width = width
    ));
    for ci in tree.children_of(n.id) {
        dump_node(tree, ci, depth + 1, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lenient_hex_variants() {
        assert_eq!(
            parse_lenient_hex("DE AD BE EF").unwrap(),
            vec![0xDE, 0xAD, 0xBE, 0xEF]
        );
        assert_eq!(
            parse_lenient_hex("DEADBEEF").unwrap(),
            vec![0xDE, 0xAD, 0xBE, 0xEF]
        );
        assert_eq!(
            parse_lenient_hex("0xDEADBEEF").unwrap(),
            vec![0xDE, 0xAD, 0xBE, 0xEF]
        );
        assert_eq!(parse_lenient_hex("1 2 3").unwrap(), vec![0x01, 0x02, 0x03]);
        assert_eq!(parse_lenient_hex("0x100").unwrap(), vec![0x01, 0x00]);
        assert!(parse_lenient_hex("DEZD").is_err());
        assert!(parse_lenient_hex("   ").is_err());
    }

    #[test]
    fn round_trip_remaps_ids() {
        use crate::core::kind::NodeKind;
        let mut tree = NodeTree::new();
        let s = tree.add_node(Node {
            kind: NodeKind::Struct,
            ..Node::default()
        });
        let sid = tree.nodes[s].id;
        let c = tree.add_node(Node {
            parent_id: sid,
            kind: NodeKind::UInt32,
            ..Node::default()
        });
        let _ = c;
        let (blob, text) = serialize(&tree, &[sid], &HashSet::new());
        // plainDump uses kindToString (the JSON/UI name "UInt32"), matching the
        // C++ `dumpNode` (clipboard.h:229).
        assert!(text.contains("UInt32"), "dump was: {text}");
        let mut dest = NodeTree::new();
        let res = deserialize(&mut dest, &blob);
        assert_eq!(res.nodes.len(), 2);
        assert_eq!(res.root_ids.len(), 1);
        // ids must have been re-minted (no collision with dest's id space).
        assert!(res.nodes.iter().all(|n| n.id != 0));
    }

    /// `dumpNode` must render the offset like Qt's `QString::arg(off,4,16,'0')`,
    /// i.e. sign-magnitude with the magnitude zero-padded inside the width-4
    /// field (the sign takes one slot) — not Rust's two's-complement `{:04x}`.
    #[test]
    fn dump_offset_is_qt_sign_magnitude() {
        use crate::core::kind::NodeKind;
        let cases = [
            (-1i32, "+0x-001"),
            (-255, "+0x-0ff"),
            (-16, "+0x-010"),
            (-4096, "+0x-1000"), // magnitude already >= 4 digits: no extra pad
            (0, "+0x0000"),
            (1, "+0x0001"),
            (255, "+0x00ff"),
            (4096, "+0x1000"),
        ];
        for (off, expected) in cases {
            let mut tree = NodeTree::new();
            let n = tree.add_node(Node {
                kind: NodeKind::Hex64,
                offset: off,
                ..Node::default()
            });
            let id = tree.nodes[n].id;
            let dump = plain_dump(&tree, &[id]);
            assert!(
                dump.starts_with(expected),
                "offset {off}: expected prefix {expected:?}, got {dump:?}"
            );
        }
    }
}
