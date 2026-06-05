//! The auto-refresh / snapshot / read-loop / adaptive-timer subsystem
//! (`controller.cpp:6430-6891`) — the refresh state machine extracted from
//! controller.rs into a child module. As a descendant of `controller` it keeps
//! full access to RcxController's private fields, methods, and module consts.

use std::collections::HashSet;
use std::sync::Arc;

use super::{
    K_IDLE_BACKOFF_TICKS, K_MAX_MAIN_EXTENT, K_PAGE_MASK, K_POINTER_SNAPSHOT_BYTE_BUDGET,
    K_STABILITY_THRESHOLD,
};
use super::*;
use crate::provider::{Provider, SnapshotProvider, K_PAGE_SIZE};

impl super::RcxController {
    /// `onRefreshTick()` (`controller.cpp:6586`) — returns the read plan.
    pub fn on_refresh_tick(&mut self) -> RefreshPlan {
        // Liveness-flip detection runs BEFORE early returns.
        let now_live = self.doc.provider.is_valid();
        if now_live != self.last_live {
            self.last_live = now_live;
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

        let extent = self.compute_data_extent();
        if extent <= 0 {
            return RefreshPlan::None;
        }

        // Ranges: main struct + pointer targets.
        let base = self.doc.tree.base_address;
        let mut ranges: Vec<(u64, i32)> = vec![(base, extent)];
        if self.snapshot.is_some() {
            let mut root_id = self.view_root_id;
            if root_id == 0 && !self.doc.tree.nodes.is_empty() {
                root_id = self.doc.tree.nodes[0].id;
            }
            let mut visited: HashSet<(u64, u64)> = HashSet::new();
            let mut budget = K_POINTER_SNAPSHOT_BYTE_BUDGET - extent as i64;
            self.collect_pointer_ranges(
                root_id,
                base,
                0,
                99,
                &mut visited,
                &mut ranges,
                &mut budget,
            );
        }

        let first_snapshot = self.snapshot.is_none() || self.prev_pages.is_empty();
        let viewport = if !first_snapshot {
            self.viewport_address_range()
        } else {
            None
        };

        const OVERSCAN_PAGES: u64 = 2;
        let mut request_pages: HashSet<u64> = HashSet::new();
        for &(range_start, range_len) in &ranges {
            let page_start = range_start & K_PAGE_MASK;
            let end = range_start.wrapping_add(range_len as u64);
            let page_end = (end + K_PAGE_SIZE - 1) & K_PAGE_MASK;
            let mut p = page_start;
            while p < page_end {
                let is_permanent = self.snapshot.as_ref().map_or(false, |s| s.is_permanent(p));
                if is_permanent {
                    p += K_PAGE_SIZE;
                    continue;
                }
                if let Some((vlo, vhi)) = viewport {
                    let lo = if vlo > OVERSCAN_PAGES * K_PAGE_SIZE {
                        (vlo - OVERSCAN_PAGES * K_PAGE_SIZE) & K_PAGE_MASK
                    } else {
                        0
                    };
                    let hi = ((vhi + OVERSCAN_PAGES * K_PAGE_SIZE) + K_PAGE_SIZE - 1) & K_PAGE_MASK;
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
                request_pages.insert(p);
                p += K_PAGE_SIZE;
            }
        }

        if request_pages.is_empty() {
            self.idle_ticks += 1;
            self.apply_adaptive_interval();
            return RefreshPlan::None;
        }

        self.read_in_flight = true;
        self.read_gen = self.refresh_gen;
        RefreshPlan::Read {
            pages: request_pages.into_iter().collect(),
            provider: self.doc.provider.clone(),
        }
    }

    /// The background read worker body (`controller.cpp:6688`). Pure — captures
    /// only the `Arc<dyn Provider>` + page list. Each page padded to 4096.
    pub fn read_pages(provider: &Arc<dyn Provider + Send + Sync>, pages: &[u64]) -> PageMap {
        let mut out = PageMap::new();
        out.reserve(pages.len());
        for &p in pages {
            let mut bytes = provider.read_bytes(p, K_PAGE_SIZE as i32);
            bytes.resize(K_PAGE_SIZE as usize, 0);
            out.insert(p, bytes);
        }
        out
    }

    /// `onReadComplete()` (`controller.cpp:6698`).
    pub fn on_read_complete(&mut self, new_pages: PageMap) {
        self.read_in_flight = false;

        if self.read_gen != self.refresh_gen {
            return;
        }

        // All-zero page-0 guard.
        if !self.prev_pages.is_empty() {
            if let Some(p0) = new_pages.get(&0) {
                if p0.iter().all(|&b| b == 0) {
                    tracing::debug!("[Refresh] discarding all-zero page-0, keeping stale snapshot");
                    return;
                }
            }
        }

        // Diff + stability.
        self.changed_offsets.clear();
        let mut any_changed = false;
        let first_snapshot = self.prev_pages.is_empty();
        for (&page_addr, fresh) in &new_pages {
            match self.prev_pages.get(&page_addr) {
                None => {
                    self.page_stability.insert(page_addr, 0);
                }
                Some(prev) => {
                    let cmp_len = prev.len().min(fresh.len());
                    let mut page_changed = false;
                    for i in 0..cmp_len {
                        if prev[i] != fresh[i] {
                            self.changed_offsets.insert(page_addr as i64 + i as i64);
                            page_changed = true;
                        }
                    }
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
            self.idle_ticks = 0;
        } else if !first_snapshot {
            self.idle_ticks += 1;
        }
        self.apply_adaptive_interval();

        let main_extent = self.compute_data_extent();

        // Accumulate prev_pages, then merge into / create the snapshot.
        for (k, v) in &new_pages {
            self.prev_pages.insert(*k, v.clone());
        }
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

        if any_changed || first_snapshot {
            self.refresh();
        }
        self.changed_offsets.clear();
    }

    /// `collectPointerRanges(...)` (`controller.cpp:6532`).
    #[allow(clippy::too_many_arguments)]
    fn collect_pointer_ranges(
        &self,
        struct_id: u64,
        mem_base: u64,
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

        let span = self.doc.tree.struct_span(struct_id);
        if span <= 0 {
            return;
        }
        ranges.push((mem_base, span));
        *budget -= span as i64;
        if *budget <= 0 {
            return;
        }

        let Some(snap) = self.snapshot.as_ref() else {
            return;
        };

        let children = self.doc.tree.children_of(struct_id);
        for &ci in &children {
            if *budget <= 0 {
                break;
            }
            let child = &self.doc.tree.nodes[ci];
            if !matches!(child.kind, NodeKind::Pointer32 | NodeKind::Pointer64) {
                continue;
            }
            if child.collapsed || child.ref_id == 0 {
                continue;
            }
            let ptr_addr = mem_base + child.offset as u64;
            let ptr_size = child.byte_size();
            if !snap.is_readable(ptr_addr, ptr_size) {
                continue;
            }
            let ptr_val = if child.kind == NodeKind::Pointer32 {
                snap.read_u32(ptr_addr) as u64
            } else {
                snap.read_u64(ptr_addr)
            };
            if ptr_val == 0 || ptr_val == u64::MAX {
                continue;
            }
            let ref_id = child.ref_id;
            self.collect_pointer_ranges(
                ref_id,
                ptr_val,
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
            if sn.kind == NodeKind::Struct && sn.ref_id != 0 && children.is_empty() {
                let ref_id = sn.ref_id;
                self.collect_pointer_ranges(
                    ref_id, mem_base, depth, max_depth, visited, ranges, budget,
                );
            }
        }
    }

    /// `viewportAddressRange()` (`controller.cpp:6798`).
    fn viewport_address_range(&self) -> Option<(u64, u64)> {
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

    /// `classifyPermanentPages(fresh)` (`controller.cpp:6832`).
    fn classify_permanent_pages(&mut self, fresh: &PageMap) {
        if self.snapshot.is_none() {
            return;
        }
        let regions: Vec<MemoryRegion> = self.doc.provider.enumerate_regions();
        if regions.is_empty() {
            return;
        }
        let to_mark: Vec<u64> = {
            let snap = self.snapshot.as_ref().unwrap();
            let mut marks = Vec::new();
            for (&page_addr, _) in fresh {
                if snap.is_permanent(page_addr) {
                    continue;
                }
                for r in &regions {
                    if r.module_name.is_empty() {
                        continue;
                    }
                    if page_addr < r.base {
                        continue;
                    }
                    if page_addr + K_PAGE_SIZE > r.base + r.size {
                        continue;
                    }
                    if !r.executable {
                        continue;
                    }
                    marks.push(page_addr);
                    break;
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
        let mut tree_extent: i64 = 0;
        for i in 0..self.doc.tree.nodes.len() {
            let off = self.doc.tree.compute_offset(i as i32);
            if off < 0 {
                continue;
            }
            let node = &self.doc.tree.nodes[i];
            let sz = self.node_size(node);
            let end = off + sz as i64;
            if end > tree_extent {
                tree_extent = end;
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

    /// `resetSnapshot()` (`controller.cpp:6876`).
    pub(super) fn reset_snapshot(&mut self) {
        self.refresh_gen += 1;
        self.read_in_flight = false;
        self.snapshot = None;
        self.prev_pages.clear();
        self.changed_offsets.clear();
        self.value_history.clear();
        self.last_value_addr.clear();
        self.page_stability.clear();
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
                self.on_read_complete(result);
                true
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
