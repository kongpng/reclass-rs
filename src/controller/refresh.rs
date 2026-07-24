//! The auto-refresh / snapshot / read-loop / adaptive-timer subsystem
//! (`controller.cpp:6430-6891`) — the refresh state machine extracted from
//! controller.rs into a child module. As a descendant of `controller` it keeps
//! full access to RcxController's private fields, methods, and module consts.

use std::collections::HashSet;
use std::sync::Arc;

use ahash::{AHashMap, AHashSet};

use super::*;
use super::{
    K_IDLE_BACKOFF_TICKS, K_MAX_MAIN_EXTENT, K_PAGE_MASK, K_POINTER_SNAPSHOT_BYTE_BUDGET,
    K_REGION_REFRESH_TICKS, K_STABILITY_THRESHOLD,
};
use crate::provider::{Provider, SnapshotProvider, K_PAGE_SIZE};

fn flat_root_data_extent(tree: &NodeTree) -> Option<i32> {
    let mut root_idx: Option<usize> = None;
    for (idx, node) in tree.nodes.iter().enumerate() {
        if node.parent_id != 0 {
            continue;
        }
        if root_idx.is_some() || !is_container_kind(node.kind) || node.ref_id != 0 {
            return None;
        }
        root_idx = Some(idx);
    }

    let root_idx = root_idx?;
    let root = &tree.nodes[root_idx];
    let root_id = root.id;
    let root_offset = root.offset as i64;
    let mut extent = 0i64;
    if root_offset >= 0 {
        extent = root_offset + root.byte_size() as i64;
    }

    for (idx, node) in tree.nodes.iter().enumerate() {
        if idx == root_idx {
            continue;
        }
        if node.parent_id != root_id || is_container_kind(node.kind) {
            return None;
        }
        let off = root_offset + node.offset as i64;
        if off < 0 {
            continue;
        }
        extent = extent.max(off + node.byte_size() as i64);
    }

    (extent > 0).then_some(extent.min(K_MAX_MAIN_EXTENT) as i32)
}

fn module_overlaps_page(modules: &[crate::provider::ModuleEntry], page_addr: u64) -> bool {
    let page_end = page_addr.saturating_add(K_PAGE_SIZE);
    let upper = modules.partition_point(|module| module.base < page_end);
    for module in modules[..upper].iter().rev() {
        let module_end = module.base.saturating_add(module.size);
        if module_end <= page_addr {
            break;
        }
        if module.base < page_end && module_end > page_addr {
            return true;
        }
    }
    false
}

/// Word-strided page diff. Equal 8-byte words are skipped wholesale; only
/// differing words descend to byte comparisons. The emitted absolute ranges are
/// byte-for-byte equivalent to the former naive scan, including runs spanning a
/// word boundary.
fn diff_page_ranges(out: &mut Vec<(i64, i64)>, page_addr: u64, old: &[u8], fresh: &[u8]) -> bool {
    let len = old.len().min(fresh.len());
    let mut changed = false;
    let mut run_start: Option<usize> = None;
    let mut i = 0usize;

    let mut visit =
        |idx: usize, differs: bool, out: &mut Vec<(i64, i64)>, run: &mut Option<usize>| {
            if differs {
                changed = true;
                if run.is_none() {
                    *run = Some(idx);
                }
            } else if let Some(start) = run.take() {
                out.push((
                    page_addr as i64 + start as i64,
                    page_addr as i64 + idx as i64,
                ));
            }
        };

    while i + 8 <= len {
        let a = u64::from_ne_bytes(old[i..i + 8].try_into().unwrap());
        let b = u64::from_ne_bytes(fresh[i..i + 8].try_into().unwrap());
        if a == b {
            if let Some(start) = run_start.take() {
                out.push((page_addr as i64 + start as i64, page_addr as i64 + i as i64));
            }
        } else {
            for byte in 0..8 {
                visit(
                    i + byte,
                    old[i + byte] != fresh[i + byte],
                    out,
                    &mut run_start,
                );
            }
        }
        i += 8;
    }
    while i < len {
        visit(i, old[i] != fresh[i], out, &mut run_start);
        i += 1;
    }
    if let Some(start) = run_start {
        out.push((
            page_addr as i64 + start as i64,
            page_addr as i64 + len as i64,
        ));
    }
    changed
}

impl super::RcxController {
    fn pointer_snapshot_children(&mut self, parent_id: u64) -> PointerSnapshotChildren {
        let generation = self.doc.tree.generation();
        let rebuild = !matches!(
            self.pointer_snapshot_child_cache.as_ref(),
            Some((cached_generation, _)) if *cached_generation == generation
        );

        if rebuild {
            self.pointer_snapshot_child_cache = Some((generation, AHashMap::new()));
        }

        if let Some(children) = self
            .pointer_snapshot_child_cache
            .as_ref()
            .and_then(|(_, by_parent)| by_parent.get(&parent_id))
        {
            return children.clone();
        }

        let children = self.doc.tree.with_children(parent_id, |indices| {
            let mut children = PointerSnapshotChildren {
                has_children: !indices.is_empty(),
                pointer_children: Vec::new(),
            };
            for &idx in indices {
                let node = &self.doc.tree.nodes[idx];
                if matches!(node.kind, NodeKind::Pointer32 | NodeKind::Pointer64)
                    && !node.collapsed
                    && node.ref_id != 0
                {
                    children.pointer_children.push(idx);
                }
            }
            children
        });

        if let Some((_, by_parent)) = self.pointer_snapshot_child_cache.as_mut() {
            by_parent.insert(parent_id, children.clone());
        }

        children
    }

    /// `onRefreshTick()` (`controller.cpp:6586`) — returns the read plan.
    pub fn on_refresh_tick(&mut self) -> RefreshPlan {
        // Liveness-flip detection runs BEFORE early returns.
        let now_live = self.doc.provider.is_valid();
        if now_live != self.last_live {
            self.last_live = now_live;
            self.source_status_dirty = true;
            self.emit(ControllerEvent::SourceLivenessChanged(now_live));
        }

        if self.read_in_flight {
            return RefreshPlan::None;
        }
        if !self.doc.provider.is_live() {
            return RefreshPlan::None;
        }
        if self.suppress_refresh {
            return RefreshPlan::None;
        }
        if self.editor.as_ref().map_or(false, |e| e.is_editing()) {
            return RefreshPlan::None;
        }

        self.tick_count += 1;

        let extent = self.cached_data_extent();
        if extent <= 0 {
            return RefreshPlan::None;
        }

        // Ranges: main struct + pointer targets.
        let base = self.doc.tree.base_address;
        let mut ranges: Vec<(u64, i32)> = Vec::new();
        if self.snapshot.is_some() {
            let mut root_id = self.view_root_id;
            if root_id == 0 && !self.doc.tree.nodes.is_empty() {
                root_id = self.doc.tree.nodes[0].id;
            }
            let root_children = self.pointer_snapshot_children(root_id);
            let needs_embedded_ref_walk = !root_children.has_children
                && self
                    .doc
                    .tree
                    .index_of_id(root_id)
                    .try_into()
                    .ok()
                    .and_then(|idx: usize| self.doc.tree.nodes.get(idx))
                    .is_some_and(|node| node.kind == NodeKind::Struct && node.ref_id != 0);
            if root_children.pointer_children.is_empty() && !needs_embedded_ref_walk {
                ranges.push((base, extent));
            } else {
                let mut visited: HashSet<(u64, u64)> = HashSet::new();
                let mut budget = K_POINTER_SNAPSHOT_BYTE_BUDGET;
                self.collect_pointer_ranges(
                    root_id,
                    base,
                    Some(extent),
                    0,
                    99,
                    &mut visited,
                    &mut ranges,
                    &mut budget,
                );
            }
        }
        if ranges.is_empty() {
            ranges.push((base, extent));
        }

        let first_snapshot = self.snapshot.is_none() || self.prev_pages.is_empty();
        let viewport = if !first_snapshot {
            self.viewport_address_range()
        } else {
            None
        };

        const OVERSCAN_PAGES: u64 = 2;
        let mut request_pages: Vec<u64> = Vec::new();
        {
            let permanent_pages = self.snapshot.as_ref().map(|s| s.permanent_pages());
            for &(range_start, range_len) in &ranges {
                let page_start = range_start & K_PAGE_MASK;
                let end = range_start.wrapping_add(range_len as u64);
                let page_end = (end + K_PAGE_SIZE - 1) & K_PAGE_MASK;
                let mut p = page_start;
                while p < page_end {
                    if permanent_pages
                        .as_ref()
                        .is_some_and(|pages| pages.contains(&p))
                    {
                        p += K_PAGE_SIZE;
                        continue;
                    }
                    if let Some((vlo, vhi)) = viewport {
                        let lo = if vlo > OVERSCAN_PAGES * K_PAGE_SIZE {
                            (vlo - OVERSCAN_PAGES * K_PAGE_SIZE) & K_PAGE_MASK
                        } else {
                            0
                        };
                        let hi =
                            ((vhi + OVERSCAN_PAGES * K_PAGE_SIZE) + K_PAGE_SIZE - 1) & K_PAGE_MASK;
                        let in_viewport = p >= lo && p < hi;
                        let is_main_range = range_start == base;
                        if is_main_range && !in_viewport {
                            let stab = self.page_stability.get(&p).copied().unwrap_or(0);
                            let is_stable = stab >= K_STABILITY_THRESHOLD;
                            if is_stable && (self.tick_count & 1) == 1 {
                                p += K_PAGE_SIZE;
                                continue;
                            }
                        }
                    }
                    request_pages.push(p);
                    p += K_PAGE_SIZE;
                }
            }
        }

        if request_pages.is_empty() {
            self.idle_ticks += 1;
            self.apply_adaptive_interval();
            return RefreshPlan::None;
        }
        if ranges.len() > 1 {
            request_pages.sort_unstable();
            request_pages.dedup();
        }

        self.read_in_flight = true;
        self.read_gen = self.refresh_gen;
        RefreshPlan::Read {
            pages: request_pages,
            provider: self.doc.provider.clone(),
        }
    }

    /// The background read worker body (`controller.cpp:6688`). Pure — captures
    /// only the `Arc<dyn Provider>` + page list. Each page padded to 4096.
    pub fn read_pages(provider: &Arc<dyn Provider + Send + Sync>, pages: &[u64]) -> PageMap {
        provider.read_pages(pages)
    }

    /// `onReadComplete()` (`controller.cpp:6698`).
    pub fn on_read_complete(&mut self, new_pages: PageMap) -> bool {
        self.read_in_flight = false;

        if self.read_gen != self.refresh_gen {
            return false;
        }

        // All-zero page-0 guard.
        if !self.prev_pages.is_empty() {
            if let Some(p0) = new_pages.get(&0) {
                if p0.iter().all(|&b| b == 0) {
                    tracing::debug!("[Refresh] discarding all-zero page-0, keeping stale snapshot");
                    let status_changed = self.last_read_ok;
                    self.last_read_ok = false;
                    return status_changed;
                }
            }
        }
        let status_changed = !self.last_read_ok;
        self.last_read_ok = true;

        // Diff + stability.
        self.changed_ranges.clear();
        let mut any_changed = false;
        let first_snapshot = self.prev_pages.is_empty();
        for (&page_addr, fresh) in &new_pages {
            match self.prev_pages.get(&page_addr) {
                None => {
                    self.page_stability.insert(page_addr, 0);
                }
                Some(prev) => {
                    let cmp_len = prev.len().min(fresh.len());
                    let page_changed = diff_page_ranges(
                        &mut self.changed_ranges,
                        page_addr,
                        &prev[..cmp_len],
                        &fresh[..cmp_len],
                    );
                    if page_changed {
                        self.page_stability.insert(page_addr, 0);
                        any_changed = true;
                    } else {
                        let prev_stab = self.page_stability.get(&page_addr).copied().unwrap_or(0);
                        self.page_stability
                            .insert(page_addr, (K_STABILITY_THRESHOLD + 16).min(prev_stab + 1));
                    }
                }
            }
        }
        if any_changed {
            normalize_changed_ranges(&mut self.changed_ranges);
        }

        if any_changed {
            self.idle_ticks = 0;
        } else if !first_snapshot {
            self.idle_ticks += 1;
        }
        self.apply_adaptive_interval();

        let main_extent = self.cached_data_extent();

        // Merge into / create the snapshot, then move the freshly-read pages
        // into the raw read baseline. The snapshot keeps its own copy because
        // local writes patch snapshot pages, while `prev_pages` must remain the
        // last bytes read from the live provider for change detection.
        match self.snapshot.as_mut() {
            Some(s) => s.merge_pages(&new_pages, main_extent),
            None => {
                self.snapshot = Some(Box::new(SnapshotProvider::new(
                    Some(self.doc.provider.clone()),
                    new_pages.clone(),
                    main_extent,
                )));
            }
        }

        // Speedup 4: classify permanent pages (after snapshot exists).
        self.classify_permanent_pages(&new_pages);
        self.prev_pages.extend(new_pages);

        let mut output_changed = false;
        if first_snapshot {
            self.refresh();
            output_changed = true;
        } else if any_changed {
            if self.changed_ranges_touch_visible_lines(&self.changed_ranges) {
                self.refresh();
                output_changed = true;
            } else {
                self.deferred_changed_ranges
                    .extend(self.changed_ranges.iter().copied());
                normalize_changed_ranges(&mut self.deferred_changed_ranges);
            }
        }
        self.changed_ranges.clear();
        output_changed || status_changed
    }

    /// `collectPointerRanges(...)` (`controller.cpp:6532`).
    #[allow(clippy::too_many_arguments)]
    fn collect_pointer_ranges(
        &mut self,
        struct_id: u64,
        mem_base: u64,
        known_span: Option<i32>,
        depth: i32,
        max_depth: i32,
        visited: &mut HashSet<(u64, u64)>,
        ranges: &mut Vec<(u64, i32)>,
        budget: &mut i64,
    ) {
        if depth >= max_depth {
            return;
        }
        if *budget <= 0 {
            return;
        }
        let key = (struct_id, mem_base);
        if visited.contains(&key) {
            return;
        }
        visited.insert(key);

        let span = known_span.unwrap_or_else(|| self.doc.tree.struct_span(struct_id));
        if span <= 0 {
            return;
        }
        ranges.push((mem_base, span));
        *budget -= span as i64;
        if *budget <= 0 {
            return;
        }

        let children = self.pointer_snapshot_children(struct_id);
        let has_children = children.has_children;
        for ci in children.pointer_children {
            if *budget <= 0 {
                break;
            }
            let Some((kind, offset, ptr_size, ref_id)) = self
                .doc
                .tree
                .nodes
                .get(ci)
                .map(|child| (child.kind, child.offset, child.byte_size(), child.ref_id))
            else {
                continue;
            };
            let ptr_addr = mem_base + offset as u64;
            let ptr_val = {
                let Some(snap) = self.snapshot.as_ref() else {
                    return;
                };
                if !snap.is_readable(ptr_addr, ptr_size) {
                    continue;
                }
                if kind == NodeKind::Pointer32 {
                    snap.read_u32(ptr_addr) as u64
                } else {
                    snap.read_u64(ptr_addr)
                }
            };
            if ptr_val == 0 || ptr_val == u64::MAX {
                continue;
            }
            self.collect_pointer_ranges(
                ref_id,
                ptr_val,
                None,
                depth + 1,
                max_depth,
                visited,
                ranges,
                budget,
            );
        }

        // Embedded struct reference.
        let idx = self.doc.tree.index_of_id(struct_id);
        if idx >= 0 {
            let sn = &self.doc.tree.nodes[idx as usize];
            if sn.kind == NodeKind::Struct && sn.ref_id != 0 && !has_children {
                let ref_id = sn.ref_id;
                self.collect_pointer_ranges(
                    ref_id, mem_base, None, depth, max_depth, visited, ranges, budget,
                );
            }
        }
    }

    /// `viewportAddressRange()` (`controller.cpp:6798`).
    fn viewport_address_range(&self) -> Option<(u64, u64)> {
        if let Some((first, last)) = self.visible_line_range {
            return self.viewport_address_range_for_lines(first, last);
        }

        let editor = self.editor.as_ref()?;
        let mut any = false;
        let mut lo = u64::MAX;
        let mut hi = 0u64;
        let first = editor.first_visible_line();
        let on_screen = editor.lines_on_screen();
        let mut v = first;
        while v < first + on_screen {
            let doc_line = editor.doc_line_from_visible(v);
            if let Some(addr) = editor.offset_addr_for_line(doc_line) {
                if addr != 0 {
                    if addr < lo {
                        lo = addr;
                    }
                    let hi_candidate = addr + 16;
                    if hi_candidate > hi {
                        hi = hi_candidate;
                    }
                    any = true;
                }
            }
            v += 1;
        }
        if !any {
            None
        } else {
            Some((lo, hi))
        }
    }

    fn viewport_address_range_for_lines(&self, first: usize, last: usize) -> Option<(u64, u64)> {
        if first > last {
            return None;
        }
        let mut any = false;
        let mut lo = u64::MAX;
        let mut hi = 0u64;
        let max_line = self.last_result.meta.len().saturating_sub(1);
        let end = last.min(max_line);
        for doc_line in first..=end {
            let Some(lm) = self.last_result.meta.get(doc_line) else {
                continue;
            };
            let addr = lm.offset_addr;
            if addr == 0 {
                continue;
            }
            lo = lo.min(addr);
            hi = hi.max(addr.saturating_add(lm.line_byte_count.max(16) as u64));
            any = true;
        }
        any.then_some((lo, hi))
    }

    /// `classifyPermanentPages(fresh)` (`controller.cpp:6832`).
    fn classify_permanent_pages(&mut self, fresh: &PageMap) {
        if self.snapshot.is_none() || fresh.is_empty() {
            return;
        }
        if !self.classify_modules_valid {
            self.classify_modules = self.doc.provider.modules_cached();
            self.classify_modules
                .sort_unstable_by_key(|module| module.base);
            self.classify_modules_valid = true;
        }
        if !self.classify_modules.is_empty() {
            if !fresh
                .keys()
                .any(|&page_addr| module_overlaps_page(&self.classify_modules, page_addr))
            {
                return;
            }
        }
        if !self.classify_regions_valid
            || self.tick_count.saturating_sub(self.classify_regions_tick) >= K_REGION_REFRESH_TICKS
        {
            self.classify_regions = self
                .doc
                .provider
                .enumerate_regions_with_modules(&self.classify_modules)
                .into_iter()
                .filter(|r| !r.module_name.is_empty() && r.executable)
                .collect();
            self.classify_regions.sort_unstable_by_key(|r| r.base);
            self.classify_regions_valid = true;
            self.classify_regions_tick = self.tick_count;
        }
        if self.classify_regions.is_empty() {
            return;
        }
        let to_mark: Vec<u64> = {
            let snap = self.snapshot.as_ref().unwrap();
            let mut marks = Vec::new();
            for (&page_addr, _) in fresh {
                if snap.is_permanent(page_addr) {
                    continue;
                }
                let page_end = page_addr.saturating_add(K_PAGE_SIZE);
                let upper = self
                    .classify_regions
                    .partition_point(|r| r.base <= page_addr);
                for r in self.classify_regions[..upper].iter().rev() {
                    let region_end = r.base.saturating_add(r.size);
                    if region_end <= page_addr {
                        break;
                    }
                    if page_end <= region_end {
                        marks.push(page_addr);
                        break;
                    }
                }
            }
            marks
        };
        let snap = self.snapshot.as_mut().unwrap();
        for p in to_mark {
            snap.mark_permanent(p);
        }
    }

    /// `computeDataExtent()` (`controller.cpp:6856`).
    pub(super) fn compute_data_extent(&self) -> i32 {
        let tree = &self.doc.tree;
        let mut tree_extent: i64 = 0;
        if !tree.nodes.is_empty() {
            if let Some(extent) = flat_root_data_extent(tree) {
                return extent;
            }

            let mut child_map: AHashMap<u64, Vec<usize>> = AHashMap::new();
            for (i, node) in tree.nodes.iter().enumerate() {
                child_map.entry(node.parent_id).or_default().push(i);
            }

            let mut abs_offsets = vec![0i64; tree.nodes.len()];
            let mut visited = vec![false; tree.nodes.len()];
            let mut stack = Vec::new();

            if let Some(roots) = child_map.get(&0) {
                for &idx in roots {
                    abs_offsets[idx] = tree.nodes[idx].offset as i64;
                    visited[idx] = true;
                    stack.push(idx);
                }
            }
            while let Some(parent_idx) = stack.pop() {
                if let Some(children) = child_map.get(&tree.nodes[parent_idx].id) {
                    for &child_idx in children {
                        if visited[child_idx] {
                            continue;
                        }
                        abs_offsets[child_idx] =
                            abs_offsets[parent_idx] + tree.nodes[child_idx].offset as i64;
                        visited[child_idx] = true;
                        stack.push(child_idx);
                    }
                }
            }

            // Match compose's orphan/cycle tolerance: unreachable nodes become
            // roots at their own offset, then their descendants inherit from them.
            for idx in 0..tree.nodes.len() {
                if visited[idx] {
                    continue;
                }
                abs_offsets[idx] = tree.nodes[idx].offset as i64;
                visited[idx] = true;
                stack.push(idx);
                while let Some(parent_idx) = stack.pop() {
                    if let Some(children) = child_map.get(&tree.nodes[parent_idx].id) {
                        for &child_idx in children {
                            if visited[child_idx] {
                                continue;
                            }
                            abs_offsets[child_idx] =
                                abs_offsets[parent_idx] + tree.nodes[child_idx].offset as i64;
                            visited[child_idx] = true;
                            stack.push(child_idx);
                        }
                    }
                }
            }

            fn span_for(
                tree: &NodeTree,
                child_map: &AHashMap<u64, Vec<usize>>,
                id_to_idx: &AHashMap<u64, usize>,
                memo: &mut AHashMap<u64, i32>,
                visiting: &mut AHashSet<u64>,
                node_id: u64,
                depth: i32,
            ) -> i32 {
                const MAX_STRUCT_SPAN_DEPTH: i32 = 256;
                if depth > MAX_STRUCT_SPAN_DEPTH || visiting.contains(&node_id) {
                    return 0;
                }
                if let Some(span) = memo.get(&node_id) {
                    return *span;
                }
                let Some(&idx) = id_to_idx.get(&node_id) else {
                    return 0;
                };

                visiting.insert(node_id);
                let node = &tree.nodes[idx];
                let declared_size = node.byte_size();
                if !is_container_kind(node.kind) && node.ref_id == 0 {
                    visiting.remove(&node_id);
                    memo.insert(node_id, declared_size);
                    return declared_size;
                }

                let mut max_end: i32 = 0;
                let kids = child_map.get(&node_id).map(Vec::as_slice).unwrap_or(&[]);
                for &child_idx in kids {
                    let child = &tree.nodes[child_idx];
                    let sz = if matches!(child.kind, NodeKind::Struct | NodeKind::Array) {
                        span_for(
                            tree,
                            child_map,
                            id_to_idx,
                            memo,
                            visiting,
                            child.id,
                            depth + 1,
                        )
                    } else {
                        child.byte_size()
                    };
                    let end = child.offset as i64 + sz as i64;
                    if end > max_end as i64 {
                        max_end = end.min(i32::MAX as i64) as i32;
                    }
                }

                if kids.is_empty() && node.kind == NodeKind::Struct && node.ref_id != 0 {
                    max_end = max_end.max(span_for(
                        tree,
                        child_map,
                        id_to_idx,
                        memo,
                        visiting,
                        node.ref_id,
                        depth + 1,
                    ));
                }

                visiting.remove(&node_id);
                let span = declared_size.max(max_end);
                memo.insert(node_id, span);
                span
            }

            let mut id_to_idx: Option<AHashMap<u64, usize>> = None;
            let mut span_memo = AHashMap::new();
            let mut span_visiting = AHashSet::new();
            for (i, node) in tree.nodes.iter().enumerate() {
                let off = abs_offsets[i];
                if off < 0 {
                    continue;
                }
                let sz = if is_container_kind(node.kind) {
                    let declared_size = node.byte_size();
                    let has_materialized_children = child_map
                        .get(&node.id)
                        .is_some_and(|children| !children.is_empty());
                    if node.kind == NodeKind::Struct
                        && node.ref_id != 0
                        && !has_materialized_children
                    {
                        let id_to_idx = id_to_idx.get_or_insert_with(|| {
                            tree.nodes
                                .iter()
                                .enumerate()
                                .map(|(idx, node)| (node.id, idx))
                                .collect()
                        });
                        declared_size.max(span_for(
                            tree,
                            &child_map,
                            id_to_idx,
                            &mut span_memo,
                            &mut span_visiting,
                            node.id,
                            0,
                        ))
                    } else {
                        declared_size
                    }
                } else {
                    node.byte_size()
                };
                let end = off + sz as i64;
                if end > tree_extent {
                    tree_extent = end;
                }
            }
        }

        if tree_extent > 0 {
            return tree_extent.min(K_MAX_MAIN_EXTENT) as i32;
        }
        let prov_size = self.doc.provider.size();
        if prov_size > 0 {
            return prov_size;
        }
        0
    }

    pub(super) fn cached_data_extent(&mut self) -> i32 {
        if self.doc.tree.nodes.is_empty() {
            return self.compute_data_extent();
        }

        let generation = self.doc.tree.generation();
        if let Some((cached_generation, extent)) = self.data_extent_cache {
            if cached_generation == generation {
                return extent;
            }
        }

        let extent = self.compute_data_extent();
        self.data_extent_cache = Some((generation, extent));
        extent
    }

    /// `resetSnapshot()` (`controller.cpp:6876`).
    pub(super) fn reset_snapshot(&mut self) {
        self.refresh_gen += 1;
        self.read_in_flight = false;
        self.snapshot = None;
        self.prev_pages.clear();
        self.changed_ranges.clear();
        self.value_history.clear();
        self.last_value_addr.clear();
        self.last_value_bytes.clear();
        self.page_stability.clear();
        self.classify_modules.clear();
        self.classify_modules_valid = false;
        self.classify_regions.clear();
        self.classify_regions_valid = false;
        self.classify_regions_tick = 0;
        self.idle_ticks = 0;
        self.tick_count = 0;
        self.apply_adaptive_interval();
    }

    /// Test-only helper: drive a full tick→read→complete cycle synchronously.
    /// Returns `true` if a read was launched and completed.
    pub fn pump_refresh(&mut self) -> bool {
        match self.on_refresh_tick() {
            RefreshPlan::None => false,
            RefreshPlan::Read { pages, provider } => {
                let result = RcxController::read_pages(&provider, &pages);
                let _ = self.on_read_complete(result);
                true
            }
        }
    }

    /// Drive one tick synchronously and report whether visible/composed output
    /// changed. Unlike [`pump_refresh`](Self::pump_refresh), unchanged or deferred
    /// offscreen reads return `false` so UI callers can skip repaint work.
    pub fn pump_refresh_output_changed(&mut self) -> bool {
        let plan = self.on_refresh_tick();
        // Liveness detection deliberately runs before every early return in
        // `on_refresh_tick`. Preserve that transition as visible output even if
        // the provider is static/suppressed, has no extent, or returns unchanged
        // pages; the editor's notify then repaints sibling source/status chrome.
        let source_status_changed = std::mem::take(&mut self.source_status_dirty);
        match plan {
            RefreshPlan::None => source_status_changed,
            RefreshPlan::Read { pages, provider } => {
                let result = RcxController::read_pages(&provider, &pages);
                self.on_read_complete(result) || source_status_changed
            }
        }
    }
}

#[cfg(test)]
mod diff_tests {
    use super::diff_page_ranges;

    fn naive(page: u64, old: &[u8], fresh: &[u8]) -> Vec<(i64, i64)> {
        let len = old.len().min(fresh.len());
        let mut out = Vec::new();
        let mut i = 0usize;
        while i < len {
            if old[i] == fresh[i] {
                i += 1;
                continue;
            }
            let start = i;
            i += 1;
            while i < len && old[i] != fresh[i] {
                i += 1;
            }
            out.push((page as i64 + start as i64, page as i64 + i as i64));
        }
        out
    }

    #[test]
    fn word_strided_diff_matches_naive_across_lengths_and_patterns() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        for len in (0..=96).chain([255, 256, 257, 4095, 4096, 4103]) {
            for _ in 0..32 {
                let mut old = vec![0u8; len];
                let mut fresh = vec![0u8; len];
                for (a, b) in old.iter_mut().zip(&mut fresh) {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    *a = (seed >> 24) as u8;
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    *b = if seed & 3 == 0 {
                        *a
                    } else {
                        (seed >> 32) as u8
                    };
                }
                let page = 0x1234_5000;
                let want = naive(page, &old, &fresh);
                let mut got = Vec::new();
                let changed = diff_page_ranges(&mut got, page, &old, &fresh);
                assert_eq!(got, want, "len={len}");
                assert_eq!(changed, !want.is_empty(), "len={len}");
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Adaptive interval + window state (`controller.cpp:6430-6478`)
// ─────────────────────────────────────────────────────────────────────────────

impl super::RcxController {
    /// `setRefreshInterval(ms)` (`controller.cpp:6430`).
    pub fn set_refresh_interval(&mut self, ms: i32) {
        self.refresh_interval_base_ms = ms.max(1);
        self.refresh_interval_max_ms = self.refresh_interval_base_ms.max(1500);
        self.refresh_interval_blur_ms = self.refresh_interval_base_ms.max(1500);
        self.apply_adaptive_interval();
    }

    /// `applyAdaptiveInterval()` (`controller.cpp:6441`).
    pub(super) fn apply_adaptive_interval(&mut self) {
        if !self.window_visible {
            self.timer_active = false;
            return;
        }
        let target = if !self.window_focused {
            self.refresh_interval_blur_ms
        } else if self.idle_ticks >= K_IDLE_BACKOFF_TICKS {
            let shift =
                (4).min((self.idle_ticks - K_IDLE_BACKOFF_TICKS) / K_IDLE_BACKOFF_TICKS + 1);
            let factor = 1i32 << shift;
            self.refresh_interval_max_ms
                .min(self.refresh_interval_base_ms * factor)
        } else {
            self.refresh_interval_base_ms
        };
        if target != self.current_interval_ms {
            self.current_interval_ms = target;
        }
        self.timer_active = true;
    }

    /// `setWindowState(focused, visible)` (`controller.cpp:6469`).
    pub fn set_window_state(&mut self, focused: bool, visible: bool) {
        let focus_gained = focused && !self.window_focused;
        self.window_focused = focused;
        self.window_visible = visible;
        if focus_gained {
            self.idle_ticks = 0;
        }
        self.apply_adaptive_interval();
    }
}
