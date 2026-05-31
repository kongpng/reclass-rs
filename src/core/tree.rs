//! `struct NodeTree` (the document) — flat node storage with id/child caches,
//! geometry math, structural validation, overlap detection, JSON serde.
//!
//! Faithful port of `src/core.h:408-863` plus the two out-of-line normalizers
//! from `src/compose.cpp:1747-1784`. The `mutable` C++ id/child caches become
//! `RefCell<HashMap<…>>` (interior mutability inside `&self` methods); the
//! invalidation timing is preserved because tests mutate `parent_id` directly
//! then call `invalidate_id_cache()`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use serde_json::{json, Map, Value};

use super::kind::{is_container_kind, NodeKind};
use super::node::{Bookmark, Node, K_MAX_ARRAY_LEN};

/// `struct OverlapPair` (`core.h:530-534`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct OverlapPair {
    /// lower-offset sibling.
    pub a_id: u64,
    /// overlapping sibling.
    pub b_id: u64,
    /// common parent.
    pub parent_id: u64,
}

/// `struct ValidateReport` (`core.h:458-467`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ValidateReport {
    pub orphans: i32,
    pub cycles: i32,
    pub duplicates: i32,
}

impl ValidateReport {
    pub fn summary(&self) -> String {
        format!(
            "orphans={} cycles={} duplicates={}",
            self.orphans, self.cycles, self.duplicates
        )
    }
    pub fn clean(&self) -> bool {
        self.orphans == 0 && self.cycles == 0 && self.duplicates == 0
    }
}

/// `struct NodeTree` (`core.h:408-835`).
#[derive(Debug)]
pub struct NodeTree {
    pub nodes: Vec<Node>,
    pub base_address: u64,
    pub base_address_formula: String,
    pub pointer_size: i32,
    pub initial_class: String,
    pub bookmarks: Vec<Bookmark>,
    next_id: u64,
    id_cache: RefCell<HashMap<u64, i32>>,
    child_cache: RefCell<HashMap<u64, Vec<usize>>>,
    generation: u64,
}

impl Default for NodeTree {
    fn default() -> Self {
        NodeTree {
            nodes: Vec::new(),
            base_address: 0x0040_0000,
            base_address_formula: String::new(),
            pointer_size: 8,
            initial_class: String::new(),
            bookmarks: Vec::new(),
            next_id: 1,
            id_cache: RefCell::new(HashMap::new()),
            child_cache: RefCell::new(HashMap::new()),
            generation: 1,
        }
    }
}

impl Clone for NodeTree {
    fn clone(&self) -> Self {
        NodeTree {
            nodes: self.nodes.clone(),
            base_address: self.base_address,
            base_address_formula: self.base_address_formula.clone(),
            pointer_size: self.pointer_size,
            initial_class: self.initial_class.clone(),
            bookmarks: self.bookmarks.clone(),
            next_id: self.next_id,
            // caches are an optimization, not semantics — clone empty.
            id_cache: RefCell::new(HashMap::new()),
            child_cache: RefCell::new(HashMap::new()),
            generation: self.generation,
        }
    }
}

impl NodeTree {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn bump_generation(&mut self) {
        self.generation += 1;
    }
    /// Caller-driven shape-change marker (`core.h:452`).
    pub fn touch(&mut self) {
        self.generation += 1;
    }
    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    /// `int addNode(const Node&)` (`core.h:431-443`) — returns the new index.
    pub fn add_node(&mut self, n: Node) -> usize {
        let mut copy = n;
        if copy.id == 0 {
            copy.id = self.next_id;
            self.next_id += 1;
        } else if copy.id >= self.next_id {
            self.next_id = copy.id + 1;
        }
        let idx = self.nodes.len();
        let (id, parent_id) = (copy.id, copy.parent_id);
        self.nodes.push(copy);
        // Incremental cache update only if cache non-empty (else stays lazy).
        {
            let mut idc = self.id_cache.borrow_mut();
            if !idc.is_empty() {
                idc.insert(id, idx as i32);
            }
        }
        {
            let mut cc = self.child_cache.borrow_mut();
            if !cc.is_empty() {
                cc.entry(parent_id).or_default().push(idx);
            }
        }
        self.generation += 1;
        idx
    }

    /// `uint64_t reserveId()` (`core.h:446`) — monotonic.
    pub fn reserve_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// `invalidateIdCache()` (`core.h:448`) — clears BOTH caches.
    pub fn invalidate_id_cache(&self) {
        self.id_cache.borrow_mut().clear();
        self.child_cache.borrow_mut().clear();
    }

    fn ensure_id_cache(&self) {
        let mut idc = self.id_cache.borrow_mut();
        if idc.is_empty() && !self.nodes.is_empty() {
            for (i, n) in self.nodes.iter().enumerate() {
                idc.insert(n.id, i as i32);
            }
        }
    }

    fn ensure_child_cache(&self) {
        let mut cc = self.child_cache.borrow_mut();
        if cc.is_empty() && !self.nodes.is_empty() {
            for (i, n) in self.nodes.iter().enumerate() {
                cc.entry(n.parent_id).or_default().push(i);
            }
        }
    }

    /// `int indexOfId(uint64_t) const` (`core.h:594-600`) — **-1 on miss**.
    pub fn index_of_id(&self, id: u64) -> i32 {
        self.ensure_id_cache();
        *self.id_cache.borrow().get(&id).unwrap_or(&-1)
    }

    /// `QVector<int> childrenOf(uint64_t) const` (`core.h:602-608`).
    pub fn children_of(&self, parent_id: u64) -> Vec<usize> {
        self.ensure_child_cache();
        self.child_cache
            .borrow()
            .get(&parent_id)
            .cloned()
            .unwrap_or_default()
    }

    /// `ValidateReport validate(bool repair)` (`core.h:468-516`).
    pub fn validate(&mut self, repair: bool) -> ValidateReport {
        let mut r = ValidateReport::default();
        if self.nodes.is_empty() {
            return r;
        }

        // Pass 1 — dedup ids.
        {
            let mut seen: HashMap<u64, usize> = HashMap::new();
            for i in 0..self.nodes.len() {
                if self.nodes[i].id == 0 || seen.contains_key(&self.nodes[i].id) {
                    r.duplicates += 1;
                    if repair {
                        self.nodes[i].id = self.next_id;
                        self.next_id += 1;
                    }
                }
                let id = self.nodes[i].id;
                seen.insert(id, i);
                if id >= self.next_id {
                    self.next_id = id + 1;
                }
            }
        }
        self.invalidate_id_cache();

        // Pass 2 — orphans.
        for i in 0..self.nodes.len() {
            if self.nodes[i].parent_id != 0 && self.index_of_id(self.nodes[i].parent_id) < 0 {
                r.orphans += 1;
                if repair {
                    self.nodes[i].parent_id = 0;
                }
            }
        }
        self.invalidate_id_cache();

        // Pass 3 — cycles.
        for i in 0..self.nodes.len() {
            let mut visited: HashSet<u64> = HashSet::new();
            let mut cur = i as i32;
            while cur >= 0 && (cur as usize) < self.nodes.len() {
                let nid = self.nodes[cur as usize].id;
                if visited.contains(&nid) {
                    r.cycles += 1;
                    if repair {
                        self.nodes[cur as usize].parent_id = 0;
                    }
                    break;
                }
                visited.insert(nid);
                if self.nodes[cur as usize].parent_id == 0 {
                    break;
                }
                cur = self.index_of_id(self.nodes[cur as usize].parent_id);
            }
        }
        self.invalidate_id_cache();
        r
    }

    /// `QVector<OverlapPair> findOverlaps() const` (`core.h:535-592`).
    pub fn find_overlaps(&self) -> Vec<OverlapPair> {
        let mut out = Vec::new();
        if self.nodes.is_empty() {
            return out;
        }

        // Local child map (this is a `const` method — no cache side effect).
        let mut child_map: HashMap<u64, Vec<usize>> = HashMap::new();
        for (i, n) in self.nodes.iter().enumerate() {
            child_map.entry(n.parent_id).or_default().push(i);
        }

        for (&parent_id, kids) in &child_map {
            if parent_id == 0 {
                continue; // root-level structs are independent classes.
            }
            let pi = self.index_of_id(parent_id);
            if pi >= 0 && self.nodes[pi as usize].is_union() {
                continue; // unions deliberately overlap at offset 0.
            }

            // (idx, start, end)
            let mut ranges: Vec<(usize, i64, i64)> = Vec::with_capacity(kids.len());
            for &ci in kids {
                let n = &self.nodes[ci];
                if n.is_static {
                    continue;
                }
                let sz = if matches!(n.kind, NodeKind::Struct | NodeKind::Array) {
                    self.struct_span(n.id)
                } else {
                    n.byte_size()
                };
                if sz <= 0 {
                    continue;
                }
                ranges.push((ci, n.offset as i64, n.offset as i64 + sz as i64));
            }
            ranges.sort_by_key(|r| r.1);

            for i in 0..ranges.len() {
                for j in (i + 1)..ranges.len() {
                    if ranges[j].1 >= ranges[i].2 {
                        break;
                    }
                    out.push(OverlapPair {
                        a_id: self.nodes[ranges[i].0].id,
                        b_id: self.nodes[ranges[j].0].id,
                        parent_id,
                    });
                }
            }
        }
        out
    }

    /// `uint64_t nodeIdForPath(path, sep)` (`core.h:615-645`) — 0 on any miss.
    pub fn node_id_for_path(&self, path: &str, sep: char) -> u64 {
        if path.is_empty() || self.nodes.is_empty() {
            return 0;
        }
        let segs: Vec<&str> = path.split(sep).filter(|s| !s.is_empty()).collect();
        if segs.is_empty() {
            return 0;
        }

        let mut cur: i32 = -1;
        for (i, n) in self.nodes.iter().enumerate() {
            if n.parent_id != 0 {
                continue;
            }
            if n.struct_type_name == segs[0] || n.name == segs[0] {
                cur = i as i32;
                break;
            }
        }
        if cur < 0 {
            return 0;
        }

        for seg in &segs[1..] {
            let parent_id = self.nodes[cur as usize].id;
            let mut next: i32 = -1;
            for (i, n) in self.nodes.iter().enumerate() {
                if n.parent_id != parent_id {
                    continue;
                }
                if n.name == *seg {
                    next = i as i32;
                    break;
                }
            }
            if next < 0 {
                return 0;
            }
            cur = next;
        }
        self.nodes[cur as usize].id
    }

    /// `QString fieldPath(id, sep)` (`core.h:653-668`).
    pub fn field_path(&self, id: u64, sep: char) -> String {
        let mut parts: Vec<String> = Vec::new();
        let mut seen: HashSet<u64> = HashSet::new();
        let mut cur = id;
        while cur != 0 && !seen.contains(&cur) {
            seen.insert(cur);
            let idx = self.index_of_id(cur);
            if idx < 0 {
                break;
            }
            let n = &self.nodes[idx as usize];
            let mut label = if !n.name.is_empty() {
                n.name.clone()
            } else {
                n.struct_type_name.clone()
            };
            if label.is_empty() && n.parent_id != 0 {
                label = "?".into();
            }
            if !label.is_empty() {
                parts.insert(0, label);
            }
            cur = n.parent_id;
        }
        parts.join(&sep.to_string())
    }

    /// `QVector<int> subtreeIndices(uint64_t) const` (`core.h:671-699`).
    pub fn subtree_indices(&self, node_id: u64) -> Vec<usize> {
        let idx = self.index_of_id(node_id);
        if idx < 0 {
            return Vec::new();
        }
        self.ensure_child_cache();
        let child_map = self.child_cache.borrow();
        let mut result = vec![idx as usize];
        let mut visited: HashSet<u64> = HashSet::new();
        visited.insert(node_id);
        let mut stack = vec![node_id];
        while let Some(pid) = stack.pop() {
            if let Some(kids) = child_map.get(&pid) {
                for &ci in kids {
                    let cid = self.nodes[ci].id;
                    if !visited.contains(&cid) {
                        visited.insert(cid);
                        result.push(ci);
                        stack.push(cid);
                    }
                }
            }
        }
        result
    }

    /// `int depthOf(int idx) const` (`core.h:701-714`).
    pub fn depth_of(&self, idx: i32) -> i32 {
        let mut d = 0;
        let mut visited: HashSet<u64> = HashSet::new();
        let mut cur = idx;
        while cur >= 0
            && (cur as usize) < self.nodes.len()
            && self.nodes[cur as usize].parent_id != 0
        {
            let nid = self.nodes[cur as usize].id;
            if visited.contains(&nid) {
                break;
            }
            visited.insert(nid);
            cur = self.index_of_id(self.nodes[cur as usize].parent_id);
            if cur < 0 {
                break;
            }
            d += 1;
        }
        d
    }

    /// `int64_t computeOffset(int idx) const` (`core.h:723-736`) — can be negative.
    pub fn compute_offset(&self, idx: i32) -> i64 {
        let mut total: i64 = 0;
        let mut visited: HashSet<u64> = HashSet::new();
        let mut cur = idx;
        while cur >= 0 && (cur as usize) < self.nodes.len() {
            let nid = self.nodes[cur as usize].id;
            if visited.contains(&nid) {
                break;
            }
            visited.insert(nid);
            total += self.nodes[cur as usize].offset as i64;
            if self.nodes[cur as usize].parent_id == 0 {
                break;
            }
            cur = self.index_of_id(self.nodes[cur as usize].parent_id);
        }
        total
    }

    /// `uint64_t absoluteAddress(int idx, bool* ok) const` (`core.h:742-750`).
    /// Returns `(address, ok)`. On negative offset, `ok=false` and `base_address`.
    pub fn absolute_address(&self, idx: i32) -> (u64, bool) {
        let off = self.compute_offset(idx);
        if off < 0 {
            (self.base_address, false)
        } else {
            (self.base_address + off as u64, true)
        }
    }

    /// `int structSpan(...)` (`core.h:752-787`) — recursive, cycle-safe.
    pub fn struct_span(&self, struct_id: u64) -> i32 {
        let mut visited: HashSet<u64> = HashSet::new();
        self.struct_span_inner(struct_id, &mut visited)
    }

    fn struct_span_inner(&self, struct_id: u64, visited: &mut HashSet<u64>) -> i32 {
        if visited.contains(&struct_id) {
            return 0; // cycle.
        }
        visited.insert(struct_id);

        let idx = self.index_of_id(struct_id);
        if idx < 0 {
            return 0;
        }
        let node = &self.nodes[idx as usize];
        let declared_size = node.byte_size();

        // Short-circuit: leaf with no children to walk.
        if !is_container_kind(node.kind) && node.ref_id == 0 {
            return declared_size;
        }

        let mut max_end: i32 = 0;
        let kids = self.children_of(struct_id);
        for &ci in &kids {
            let c = &self.nodes[ci];
            if c.is_static {
                continue;
            }
            let sz = if matches!(c.kind, NodeKind::Struct | NodeKind::Array) {
                self.struct_span_inner(c.id, visited)
            } else {
                c.byte_size()
            };
            let end = c.offset as i64 + sz as i64;
            if end > max_end as i64 {
                max_end = end.min(i32::MAX as i64) as i32;
            }
        }

        // Embedded struct reference.
        let node = &self.nodes[idx as usize];
        if kids.is_empty() && node.kind == NodeKind::Struct && node.ref_id != 0 {
            max_end = max_end.max(self.struct_span_inner(node.ref_id, visited));
        }

        declared_size.max(max_end)
    }

    /// Container-aware footprint (`Node::totalByteSize`, `core.h:841-845`).
    pub fn total_byte_size(&self, n: &Node) -> i32 {
        if matches!(n.kind, NodeKind::Struct | NodeKind::Array) {
            self.struct_span(n.id)
        } else {
            n.byte_size()
        }
    }

    /// `normalizePreferAncestors` (`compose.cpp:1747-1784`) — drop any node that
    /// has a *selected ancestor*; keep only the topmost selected nodes.
    pub fn normalize_prefer_ancestors(&self, ids: &HashSet<u64>) -> HashSet<u64> {
        let mut out = HashSet::new();
        for &id in ids {
            let mut has_selected_ancestor = false;
            let mut visited: HashSet<u64> = HashSet::new();
            let idx = self.index_of_id(id);
            if idx < 0 {
                continue;
            }
            let mut cur = self.nodes[idx as usize].parent_id;
            while cur != 0 && !visited.contains(&cur) {
                visited.insert(cur);
                if ids.contains(&cur) {
                    has_selected_ancestor = true;
                    break;
                }
                let ci = self.index_of_id(cur);
                if ci < 0 {
                    break;
                }
                cur = self.nodes[ci as usize].parent_id;
            }
            if !has_selected_ancestor {
                out.insert(id);
            }
        }
        out
    }

    /// `normalizePreferDescendants` (`compose.cpp:1747-1784`) — drop any node
    /// that has a *selected descendant*; keep only the deepest selected nodes.
    pub fn normalize_prefer_descendants(&self, ids: &HashSet<u64>) -> HashSet<u64> {
        let mut out = HashSet::new();
        for &id in ids {
            let mut has_selected_descendant = false;
            for &di in &self.subtree_indices(id) {
                let did = self.nodes[di].id;
                if did != id && ids.contains(&did) {
                    has_selected_descendant = true;
                    break;
                }
            }
            if !has_selected_descendant {
                out.insert(id);
            }
        }
        out
    }

    /// `NodeTree::toJson()` (`core.h:793-812`).
    pub fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert(
            "baseAddress".into(),
            json!(format!("{:x}", self.base_address)),
        );
        if !self.base_address_formula.is_empty() {
            o.insert(
                "baseAddressFormula".into(),
                json!(self.base_address_formula),
            );
        }
        if !self.initial_class.is_empty() {
            o.insert("initialClass".into(), json!(self.initial_class));
        }
        if self.pointer_size != 8 {
            o.insert("pointerSize".into(), json!(self.pointer_size));
        }
        o.insert("nextId".into(), json!(self.next_id.to_string()));
        o.insert(
            "nodes".into(),
            Value::Array(self.nodes.iter().map(Node::to_json).collect()),
        );
        if !self.bookmarks.is_empty() {
            o.insert(
                "bookmarks".into(),
                Value::Array(self.bookmarks.iter().map(Bookmark::to_json).collect()),
            );
        }
        Value::Object(o)
    }

    /// `static NodeTree fromJson()` (`core.h:814-833`).
    pub fn from_json(o: &Value) -> NodeTree {
        let mut t = NodeTree::default();
        let base_str = o
            .get("baseAddress")
            .and_then(Value::as_str)
            .unwrap_or("400000");
        t.base_address = u64::from_str_radix(base_str.trim_start_matches("0x"), 16).unwrap_or(0);
        t.base_address_formula = o
            .get("baseAddressFormula")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        t.initial_class = o
            .get("initialClass")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        t.pointer_size = o.get("pointerSize").and_then(Value::as_i64).unwrap_or(8) as i32;
        t.next_id = o
            .get("nextId")
            .and_then(Value::as_str)
            .unwrap_or("1")
            .trim()
            .parse()
            .unwrap_or(1);
        if let Some(arr) = o.get("nodes").and_then(Value::as_array) {
            t.nodes.reserve(arr.len());
            for v in arr {
                let n = Node::from_json(v);
                if n.id >= t.next_id {
                    t.next_id = n.id + 1;
                }
                t.nodes.push(n);
            }
        }
        if let Some(arr) = o.get("bookmarks").and_then(Value::as_array) {
            for v in arr {
                t.bookmarks.push(Bookmark::from_json(v));
            }
        }
        t
    }
}

/// `QStringList rootClassNames(const NodeTree&)` (`core.h:853-863`).
pub fn root_class_names(tree: &NodeTree) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in &tree.nodes {
        if n.parent_id != 0 || n.kind != NodeKind::Struct {
            continue;
        }
        let name = if !n.struct_type_name.is_empty() {
            n.struct_type_name.clone()
        } else if !n.name.is_empty() {
            n.name.clone()
        } else {
            "Untitled".into()
        };
        if !out.contains(&name) {
            out.push(name);
        }
    }
    if out.is_empty() {
        out.push("Untitled".into());
    }
    out
}

// silence the otherwise-unused clamp const re-export check in some builds
const _: i32 = K_MAX_ARRAY_LEN;

#[cfg(test)]
mod tests {
    use super::*;

    fn child(parent: u64, kind: NodeKind, offset: i32) -> Node {
        Node {
            parent_id: parent,
            kind,
            offset,
            ..Node::default()
        }
    }

    #[test]
    fn stable_ids_and_index_of_id() {
        let mut t = NodeTree::new();
        let i0 = t.add_node(Node::default());
        let i1 = t.add_node(Node::default());
        assert_eq!(i0, 0);
        assert_eq!(i1, 1);
        assert_eq!(t.nodes[0].id, 1);
        assert_eq!(t.nodes[1].id, 2);
        assert_eq!(t.index_of_id(999), -1);
    }

    #[test]
    fn struct_span_nested() {
        let mut t = NodeTree::new();
        let s = t.add_node(Node {
            kind: NodeKind::Struct,
            ..Node::default()
        });
        let sid = t.nodes[s].id;
        t.add_node(child(sid, NodeKind::UInt32, 0));
        t.add_node(child(sid, NodeKind::UInt64, 4));
        assert_eq!(t.struct_span(sid), 12);
    }

    #[test]
    fn compute_offset_and_absolute() {
        let mut t = NodeTree::new();
        t.base_address = 0x1000;
        let s = t.add_node(Node {
            kind: NodeKind::Struct,
            offset: 0,
            ..Node::default()
        });
        let sid = t.nodes[s].id;
        let c = t.add_node(child(sid, NodeKind::Struct, 16));
        let cid = t.nodes[c].id;
        let leaf = t.add_node(child(cid, NodeKind::UInt32, 8));
        assert_eq!(t.compute_offset(leaf as i32), 24);
        assert_eq!(t.absolute_address(leaf as i32), (0x1018, true));
    }

    #[test]
    fn overlap_long_over_three() {
        let mut t = NodeTree::new();
        let s = t.add_node(Node {
            kind: NodeKind::Struct,
            ..Node::default()
        });
        let sid = t.nodes[s].id;
        let big = t.add_node(child(sid, NodeKind::UInt128, 0)); // [0,16)
        t.add_node(child(sid, NodeKind::UInt32, 0));
        t.add_node(child(sid, NodeKind::UInt32, 4));
        t.add_node(child(sid, NodeKind::UInt32, 8));
        let pairs = t.find_overlaps();
        assert_eq!(pairs.len(), 3);
        let big_id = t.nodes[big].id;
        assert!(pairs.iter().all(|p| p.a_id == big_id));
    }

    #[test]
    fn json_round_trip() {
        let mut t = NodeTree::new();
        t.base_address = 0xDEAD;
        t.pointer_size = 4;
        t.add_node(Node {
            kind: NodeKind::Struct,
            ..Node::default()
        });
        t.add_node(Node {
            kind: NodeKind::UInt32,
            ..Node::default()
        });
        let back = NodeTree::from_json(&t.to_json());
        assert_eq!(back.base_address, 0xDEAD);
        assert_eq!(back.pointer_size, 4);
        assert!(back.next_id >= 3);
        assert_eq!(back.nodes.len(), 2);
    }

    #[test]
    fn normalize_prefer_ancestors_and_descendants() {
        let mut t = NodeTree::new();
        let root = t.add_node(Node {
            kind: NodeKind::Struct,
            ..Node::default()
        });
        let rid = t.nodes[root].id;
        let a = t.add_node(child(rid, NodeKind::Struct, 0));
        let aid = t.nodes[a].id;
        let leaf = t.add_node(child(aid, NodeKind::UInt32, 0));
        let lid = t.nodes[leaf].id;

        let sel: HashSet<u64> = [rid, lid].into_iter().collect();
        assert_eq!(
            t.normalize_prefer_ancestors(&sel),
            [rid].into_iter().collect()
        );

        let sel2: HashSet<u64> = [rid, aid, lid].into_iter().collect();
        assert_eq!(
            t.normalize_prefer_descendants(&sel2),
            [lid].into_iter().collect()
        );
    }
}
