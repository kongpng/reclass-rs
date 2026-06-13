//! `struct NodeTree` (the document) — flat node storage with id/child caches,
//! geometry math, structural validation, overlap detection, JSON serde.
//!
//! Faithful port of `src/core.h:408-863` plus the two out-of-line normalizers
//! from `src/compose.cpp:1747-1784`. The `mutable` C++ id/child caches become
//! `RefCell<AHashMap<…>>` (interior mutability inside `&self` methods); the
//! invalidation timing is preserved because tests mutate `parent_id` directly
//! then call `invalidate_id_cache()`.

use std::cell::RefCell;
use std::collections::HashSet;

use ahash::{AHashMap, AHashSet};

use serde_json::{json, Map, Value};

use super::kind::{is_container_kind, NodeKind};
use super::node::{
    parse_prefixed_sequence, Bookmark, EvidenceEvent, EvidenceHypothesis, EvidenceProposal, Node,
    K_MAX_ARRAY_LEN,
};

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
    /// Evidence arrays (`core.h:589-591`) — provenance of RE decisions.
    pub evidence_events: Vec<EvidenceEvent>,
    pub evidence_hypotheses: Vec<EvidenceHypothesis>,
    pub evidence_proposals: Vec<EvidenceProposal>,
    next_id: u64,
    /// `m_nextEvidenceEventId` etc. (`core.h:593-595`) — monotonic id counters.
    next_evidence_event_id: u64,
    next_evidence_hypothesis_id: u64,
    next_evidence_proposal_id: u64,
    id_cache: RefCell<AHashMap<u64, i32>>,
    child_cache: RefCell<AHashMap<u64, Vec<usize>>>,
    span_cache: RefCell<SpanCache>,
    generation: u64,
}

#[derive(Debug, Default)]
struct SpanCache {
    generation: u64,
    spans: AHashMap<u64, i32>,
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
            evidence_events: Vec::new(),
            evidence_hypotheses: Vec::new(),
            evidence_proposals: Vec::new(),
            next_id: 1,
            next_evidence_event_id: 1,
            next_evidence_hypothesis_id: 1,
            next_evidence_proposal_id: 1,
            id_cache: RefCell::new(AHashMap::new()),
            child_cache: RefCell::new(AHashMap::new()),
            span_cache: RefCell::new(SpanCache::default()),
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
            evidence_events: self.evidence_events.clone(),
            evidence_hypotheses: self.evidence_hypotheses.clone(),
            evidence_proposals: self.evidence_proposals.clone(),
            next_id: self.next_id,
            next_evidence_event_id: self.next_evidence_event_id,
            next_evidence_hypothesis_id: self.next_evidence_hypothesis_id,
            next_evidence_proposal_id: self.next_evidence_proposal_id,
            // caches are an optimization, not semantics — clone empty.
            id_cache: RefCell::new(AHashMap::new()),
            child_cache: RefCell::new(AHashMap::new()),
            span_cache: RefCell::new(SpanCache::default()),
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

    /// `m_nextEvidenceEventId` etc. read accessors (mirror the `quint64`
    /// counters at `core.h:593-595`).
    pub fn next_evidence_event_id(&self) -> u64 {
        self.next_evidence_event_id
    }
    pub fn next_evidence_hypothesis_id(&self) -> u64 {
        self.next_evidence_hypothesis_id
    }
    pub fn next_evidence_proposal_id(&self) -> u64 {
        self.next_evidence_proposal_id
    }

    /// `reserveEvidenceEventId()` (`core.h:628-630`) — `ev_N`, monotonic.
    pub fn reserve_evidence_event_id(&mut self) -> String {
        let s = format!("ev_{}", self.next_evidence_event_id);
        self.next_evidence_event_id += 1;
        s
    }
    /// `reserveEvidenceHypothesisId()` (`core.h:631-633`) — `hyp_N`.
    pub fn reserve_evidence_hypothesis_id(&mut self) -> String {
        let s = format!("hyp_{}", self.next_evidence_hypothesis_id);
        self.next_evidence_hypothesis_id += 1;
        s
    }
    /// `reserveEvidenceProposalId()` (`core.h:634-636`) — `prop_N`.
    pub fn reserve_evidence_proposal_id(&mut self) -> String {
        let s = format!("prop_{}", self.next_evidence_proposal_id);
        self.next_evidence_proposal_id += 1;
        s
    }

    /// `appendEvidenceEvent(event)` (`core.h:638-650`). Assigns an `ev_N` id
    /// when empty (else bumps the counter past an explicit id) and stamps the
    /// timestamp when unset. `now_ms` is the current epoch-msec the caller
    /// supplies (the C++ uses `QDateTime::currentMSecsSinceEpoch()`).
    pub fn append_evidence_event(
        &mut self,
        mut event: EvidenceEvent,
        now_ms: i64,
    ) -> EvidenceEvent {
        if event.id.is_empty() {
            event.id = self.reserve_evidence_event_id();
        } else {
            let seq = parse_prefixed_sequence(&event.id, "ev_");
            if seq >= self.next_evidence_event_id {
                self.next_evidence_event_id = seq + 1;
            }
        }
        if event.timestamp <= 0 {
            event.timestamp = now_ms;
        }
        self.evidence_events.push(event.clone());
        event
    }

    /// `appendEvidenceHypothesis(hyp)` (`core.h:652-664`).
    pub fn append_evidence_hypothesis(
        &mut self,
        mut hyp: EvidenceHypothesis,
        now_ms: i64,
    ) -> EvidenceHypothesis {
        if hyp.id.is_empty() {
            hyp.id = self.reserve_evidence_hypothesis_id();
        } else {
            let seq = parse_prefixed_sequence(&hyp.id, "hyp_");
            if seq >= self.next_evidence_hypothesis_id {
                self.next_evidence_hypothesis_id = seq + 1;
            }
        }
        if hyp.created_at <= 0 {
            hyp.created_at = now_ms;
        }
        if hyp.updated_at <= 0 {
            hyp.updated_at = now_ms;
        }
        self.evidence_hypotheses.push(hyp.clone());
        hyp
    }

    /// `appendEvidenceProposal(proposal)` (`core.h:666-678`).
    pub fn append_evidence_proposal(
        &mut self,
        mut proposal: EvidenceProposal,
        now_ms: i64,
    ) -> EvidenceProposal {
        if proposal.id.is_empty() {
            proposal.id = self.reserve_evidence_proposal_id();
        } else {
            let seq = parse_prefixed_sequence(&proposal.id, "prop_");
            if seq >= self.next_evidence_proposal_id {
                self.next_evidence_proposal_id = seq + 1;
            }
        }
        if proposal.created_at <= 0 {
            proposal.created_at = now_ms;
        }
        if proposal.updated_at <= 0 {
            proposal.updated_at = now_ms;
        }
        self.evidence_proposals.push(proposal.clone());
        proposal
    }

    /// `invalidateIdCache()` (`core.h:448`) — clears BOTH caches.
    pub fn invalidate_id_cache(&self) {
        self.id_cache.borrow_mut().clear();
        self.child_cache.borrow_mut().clear();
        self.span_cache.borrow_mut().spans.clear();
    }

    fn ensure_child_cache(&self) {
        if self.nodes.is_empty() || !self.child_cache.borrow().is_empty() {
            return;
        }
        let mut cc = self.child_cache.borrow_mut();
        if !cc.is_empty() {
            return;
        }
        for (i, n) in self.nodes.iter().enumerate() {
            cc.entry(n.parent_id).or_default().push(i);
        }
    }

    /// `int indexOfId(uint64_t) const` (`core.h:594-600`) — **-1 on miss**.
    pub fn index_of_id(&self, id: u64) -> i32 {
        let mut idc = self.id_cache.borrow_mut();
        if idc.is_empty() && !self.nodes.is_empty() {
            for (i, n) in self.nodes.iter().enumerate() {
                idc.insert(n.id, i as i32);
            }
        }
        *idc.get(&id).unwrap_or(&-1)
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

    pub fn with_children<R>(&self, parent_id: u64, f: impl FnOnce(&[usize]) -> R) -> R {
        self.ensure_child_cache();
        let child_cache = self.child_cache.borrow();
        match child_cache.get(&parent_id) {
            Some(children) => f(children),
            None => f(&[]),
        }
    }

    pub fn child_count(&self, parent_id: u64) -> usize {
        self.ensure_child_cache();
        self.child_cache
            .borrow()
            .get(&parent_id)
            .map_or(0, Vec::len)
    }

    pub fn has_children(&self, parent_id: u64) -> bool {
        self.ensure_child_cache();
        self.child_cache
            .borrow()
            .get(&parent_id)
            .is_some_and(|children| !children.is_empty())
    }

    /// `ValidateReport validate(bool repair)` (`core.h:468-516`).
    pub fn validate(&mut self, repair: bool) -> ValidateReport {
        let mut r = ValidateReport::default();
        if self.nodes.is_empty() {
            return r;
        }

        // Pass 1 — dedup ids.
        {
            let mut seen: AHashMap<u64, usize> = AHashMap::new();
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
            let mut visited: AHashSet<u64> = AHashSet::new();
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
        let mut child_map: AHashMap<u64, Vec<usize>> = AHashMap::new();
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
        let mut seen: AHashSet<u64> = AHashSet::new();
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
        let mut visited: AHashSet<u64> = AHashSet::new();
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
        let mut visited: AHashSet<u64> = AHashSet::new();
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
        let mut visited: AHashSet<u64> = AHashSet::new();
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
        {
            let mut cache = self.span_cache.borrow_mut();
            if cache.generation != self.generation {
                cache.generation = self.generation;
                cache.spans.clear();
            } else if let Some(span) = cache.spans.get(&struct_id) {
                return *span;
            }
        }

        let mut visited: AHashSet<u64> = AHashSet::new();
        self.struct_span_inner(struct_id, &mut visited, 0)
    }

    fn struct_span_inner(&self, struct_id: u64, visited: &mut AHashSet<u64>, depth: i32) -> i32 {
        // `visited` stops true cycles; the depth bound stops a crafted document's
        // deep *distinct* nesting (a flat node array forming a thousands-deep
        // parent/ref chain) from overflowing the stack here — the same class of
        // fix applied to `compose`. Real layouts nest far below the cap.
        const MAX_STRUCT_SPAN_DEPTH: i32 = 256;
        if depth > MAX_STRUCT_SPAN_DEPTH || visited.contains(&struct_id) {
            return 0; // cycle or pathological depth.
        }
        if let Some(span) = self.span_cache.borrow().spans.get(&struct_id) {
            return *span;
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
            self.span_cache
                .borrow_mut()
                .spans
                .insert(struct_id, declared_size);
            return declared_size;
        }

        let mut max_end: i32 = 0;
        let kids = self.children_of(struct_id);
        for &ci in &kids {
            let c = &self.nodes[ci];
            let sz = if matches!(c.kind, NodeKind::Struct | NodeKind::Array) {
                self.struct_span_inner(c.id, visited, depth + 1)
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
            max_end = max_end.max(self.struct_span_inner(node.ref_id, visited, depth + 1));
        }

        let span = declared_size.max(max_end);
        self.span_cache.borrow_mut().spans.insert(struct_id, span);
        span
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
            let mut visited: AHashSet<u64> = AHashSet::new();
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
        let mut drop_ancestors = HashSet::with_capacity(ids.len());
        for &id in ids {
            let idx = self.index_of_id(id);
            if idx < 0 {
                continue;
            }
            let mut visited: AHashSet<u64> = AHashSet::new();
            let mut cur = self.nodes[idx as usize].parent_id;
            while cur != 0 && visited.insert(cur) {
                let ci = self.index_of_id(cur);
                if ci < 0 {
                    break;
                }
                if ids.contains(&cur) && !drop_ancestors.insert(cur) {
                    break;
                }
                cur = self.nodes[ci as usize].parent_id;
            }
        }

        let mut out = HashSet::with_capacity(ids.len().saturating_sub(drop_ancestors.len()));
        for &id in ids {
            if !drop_ancestors.contains(&id) {
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
        if !self.evidence_events.is_empty() {
            o.insert(
                "evidenceEvents".into(),
                Value::Array(
                    self.evidence_events
                        .iter()
                        .map(EvidenceEvent::to_json)
                        .collect(),
                ),
            );
        }
        if !self.evidence_hypotheses.is_empty() {
            o.insert(
                "evidenceHypotheses".into(),
                Value::Array(
                    self.evidence_hypotheses
                        .iter()
                        .map(EvidenceHypothesis::to_json)
                        .collect(),
                ),
            );
        }
        if !self.evidence_proposals.is_empty() {
            o.insert(
                "evidenceProposals".into(),
                Value::Array(
                    self.evidence_proposals
                        .iter()
                        .map(EvidenceProposal::to_json)
                        .collect(),
                ),
            );
        }
        if self.next_evidence_event_id != 1 {
            o.insert(
                "nextEvidenceEventId".into(),
                json!(self.next_evidence_event_id.to_string()),
            );
        }
        if self.next_evidence_hypothesis_id != 1 {
            o.insert(
                "nextEvidenceHypothesisId".into(),
                json!(self.next_evidence_hypothesis_id.to_string()),
            );
        }
        if self.next_evidence_proposal_id != 1 {
            o.insert(
                "nextEvidenceProposalId".into(),
                json!(self.next_evidence_proposal_id.to_string()),
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
        // Evidence counters (default "1"), then arrays — bumping each counter
        // past any explicit prefixed id (`core.h:942-967`).
        t.next_evidence_event_id = o
            .get("nextEvidenceEventId")
            .and_then(Value::as_str)
            .unwrap_or("1")
            .trim()
            .parse()
            .unwrap_or(1);
        t.next_evidence_hypothesis_id = o
            .get("nextEvidenceHypothesisId")
            .and_then(Value::as_str)
            .unwrap_or("1")
            .trim()
            .parse()
            .unwrap_or(1);
        t.next_evidence_proposal_id = o
            .get("nextEvidenceProposalId")
            .and_then(Value::as_str)
            .unwrap_or("1")
            .trim()
            .parse()
            .unwrap_or(1);
        if let Some(arr) = o.get("evidenceEvents").and_then(Value::as_array) {
            t.evidence_events.reserve(arr.len());
            for v in arr {
                let event = EvidenceEvent::from_json(v);
                let seq = parse_prefixed_sequence(&event.id, "ev_");
                if seq >= t.next_evidence_event_id {
                    t.next_evidence_event_id = seq + 1;
                }
                t.evidence_events.push(event);
            }
        }
        if let Some(arr) = o.get("evidenceHypotheses").and_then(Value::as_array) {
            t.evidence_hypotheses.reserve(arr.len());
            for v in arr {
                let hyp = EvidenceHypothesis::from_json(v);
                let seq = parse_prefixed_sequence(&hyp.id, "hyp_");
                if seq >= t.next_evidence_hypothesis_id {
                    t.next_evidence_hypothesis_id = seq + 1;
                }
                t.evidence_hypotheses.push(hyp);
            }
        }
        if let Some(arr) = o.get("evidenceProposals").and_then(Value::as_array) {
            t.evidence_proposals.reserve(arr.len());
            for v in arr {
                let prop = EvidenceProposal::from_json(v);
                let seq = parse_prefixed_sequence(&prop.id, "prop_");
                if seq >= t.next_evidence_proposal_id {
                    t.next_evidence_proposal_id = seq + 1;
                }
                t.evidence_proposals.push(prop);
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
    fn has_children_uses_incremental_child_cache() {
        let mut t = NodeTree::new();
        let parent_idx = t.add_node(Node {
            kind: NodeKind::Struct,
            ..Node::default()
        });
        let parent_id = t.nodes[parent_idx].id;

        assert!(!t.has_children(parent_id));
        assert!(t.children_of(parent_id).is_empty());
        assert_eq!(t.child_count(parent_id), 0);
        assert_eq!(t.with_children(parent_id, |children| children.len()), 0);

        let child_idx = t.add_node(child(parent_id, NodeKind::UInt32, 0));
        assert!(t.has_children(parent_id));
        assert_eq!(t.child_count(parent_id), 1);
        assert_eq!(
            t.with_children(parent_id, |children| children.to_vec()),
            vec![child_idx]
        );
        assert_eq!(t.children_of(parent_id), vec![child_idx]);
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
    fn struct_span_cache_tracks_generation_and_manual_invalidation() {
        let mut t = NodeTree::new();
        let s = t.add_node(Node {
            kind: NodeKind::Struct,
            ..Node::default()
        });
        let sid = t.nodes[s].id;
        let first = t.add_node(child(sid, NodeKind::UInt32, 0));
        assert_eq!(t.struct_span(sid), 4);
        assert_eq!(t.struct_span(sid), 4);

        t.add_node(child(sid, NodeKind::UInt64, 8));
        assert_eq!(t.struct_span(sid), 16);

        t.nodes[first].offset = 32;
        t.invalidate_id_cache();
        assert_eq!(t.struct_span(sid), 36);
    }

    #[test]
    fn struct_span_deep_chain_is_bounded() {
        // A crafted document can encode a flat node array whose parent_id links
        // form a chain thousands of levels deep. Without the depth bound in
        // struct_span_inner this recurses until the stack overflows (SIGSEGV);
        // with it the walk truncates and returns. Build a chain far past both the
        // 256 cap and any plausible stack limit, and confirm the span computation
        // simply returns a finite, non-negative value.
        let mut t = NodeTree::new();
        let root = t.add_node(Node {
            kind: NodeKind::Struct,
            ..Node::default()
        });
        let mut parent_id = t.nodes[root].id;
        for _ in 0..50_000 {
            let idx = t.add_node(child(parent_id, NodeKind::Struct, 0));
            parent_id = t.nodes[idx].id;
        }
        let span = t.struct_span(t.nodes[root].id);
        assert!(span >= 0);
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

    #[test]
    fn normalize_prefer_descendants_keeps_deepest_chain_selection() {
        let mut t = NodeTree::new();
        let root = t.add_node(Node {
            kind: NodeKind::Struct,
            ..Node::default()
        });
        let mut parent_id = t.nodes[root].id;
        let mut deepest_id = parent_id;
        let mut selected: HashSet<u64> = [parent_id].into_iter().collect();
        for _ in 0..1024 {
            let idx = t.add_node(child(parent_id, NodeKind::Struct, 0));
            parent_id = t.nodes[idx].id;
            deepest_id = parent_id;
            selected.insert(parent_id);
        }

        assert_eq!(
            t.normalize_prefer_descendants(&selected),
            [deepest_id].into_iter().collect()
        );
    }

    // ── Evidence arrays ──

    #[test]
    fn evidence_arrays_round_trip_and_counters_omit_default() {
        let mut t = NodeTree::new();
        // Default counters (== 1) must NOT appear in the JSON.
        let empty = t.to_json();
        let eo = empty.as_object().unwrap();
        assert!(!eo.contains_key("evidenceEvents"));
        assert!(!eo.contains_key("nextEvidenceEventId"));
        assert!(!eo.contains_key("nextEvidenceHypothesisId"));
        assert!(!eo.contains_key("nextEvidenceProposalId"));

        // Append one of each; the append helpers stamp ids + timestamps.
        let now = 1_700_000_000_000;
        t.append_evidence_event(
            EvidenceEvent {
                summary: "marker".into(),
                ..EvidenceEvent::default()
            },
            now,
        );
        t.append_evidence_hypothesis(EvidenceHypothesis::default(), now);
        t.append_evidence_proposal(EvidenceProposal::default(), now);

        assert_eq!(t.evidence_events[0].id, "ev_1");
        assert_eq!(t.evidence_events[0].timestamp, now);
        assert_eq!(t.evidence_hypotheses[0].id, "hyp_1");
        assert_eq!(t.evidence_hypotheses[0].created_at, now);
        assert_eq!(t.evidence_proposals[0].id, "prop_1");
        assert_eq!(t.next_evidence_event_id(), 2);
        assert_eq!(t.next_evidence_hypothesis_id(), 2);
        assert_eq!(t.next_evidence_proposal_id(), 2);

        // Now the JSON carries the arrays + (bumped) counters.
        let j = t.to_json();
        let o = j.as_object().unwrap();
        assert!(o.contains_key("evidenceEvents"));
        assert_eq!(o["nextEvidenceEventId"], json!("2"));
        assert_eq!(o["nextEvidenceHypothesisId"], json!("2"));
        assert_eq!(o["nextEvidenceProposalId"], json!("2"));

        // Full round-trip preserves arrays + counters.
        let back = NodeTree::from_json(&j);
        assert_eq!(back.evidence_events, t.evidence_events);
        assert_eq!(back.evidence_hypotheses, t.evidence_hypotheses);
        assert_eq!(back.evidence_proposals, t.evidence_proposals);
        assert_eq!(back.next_evidence_event_id(), 2);
        assert_eq!(back.next_evidence_hypothesis_id(), 2);
        assert_eq!(back.next_evidence_proposal_id(), 2);
    }

    #[test]
    fn evidence_counter_bumps_past_explicit_ids_on_load() {
        // A C++-authored file with high explicit ids but no nextEvidence*Id key
        // (older save) must still bump the counter past the seen ids.
        let j = json!({
            "baseAddress": "400000",
            "nextId": "1",
            "nodes": [],
            "evidenceEvents": [ { "id": "ev_10", "timestamp": "5" } ],
            "evidenceHypotheses": [ { "id": "hyp_4", "createdAt": "1", "updatedAt": "1", "status": "open", "confidence": 0.0 } ],
            "evidenceProposals": [ { "id": "prop_7", "createdAt": "1", "updatedAt": "1", "status": "pending", "confidence": 0.0 } ],
        });
        let t = NodeTree::from_json(&j);
        assert_eq!(t.evidence_events.len(), 1);
        assert_eq!(t.next_evidence_event_id(), 11);
        assert_eq!(t.next_evidence_hypothesis_id(), 5);
        assert_eq!(t.next_evidence_proposal_id(), 8);
    }

    #[test]
    fn append_evidence_with_explicit_id_bumps_counter() {
        let mut t = NodeTree::new();
        t.append_evidence_event(
            EvidenceEvent {
                id: "ev_50".into(),
                timestamp: 1,
                ..EvidenceEvent::default()
            },
            0,
        );
        // Counter advanced past the explicit id; next auto-id is ev_51.
        assert_eq!(t.next_evidence_event_id(), 51);
        let auto = t.reserve_evidence_event_id();
        assert_eq!(auto, "ev_51");
    }
}
