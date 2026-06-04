//! Tree + Provider → rendered rows (text + `LineMeta`, column geometry).
//!
//! Faithful 1:1 port of `src/compose.cpp` (the composition / render engine) plus
//! the column-geometry / span helpers and value-rendering `fmt::` routines that
//! `compose.cpp` consumes from `core.h` / `format.cpp`.
//!
//! The composition engine is **read-only** over the tree: it walks a [`NodeTree`]
//! against a [`Provider`] and produces a [`ComposeResult`] — a single text blob
//! plus a parallel `Vec<LineMeta>` describing each rendered line (offsets, fold
//! levels, chips, column geometry, markers). Recursive, with cycle guards for
//! both struct recursion and pointer-dereference recursion.
//!
//! **Fidelity-critical:** all column / `startCol` / `endCol` / `maxLineLen` math
//! is in UTF-16 code units (Qt `QString` semantics). The glyphs the renderer
//! uses (`▸ ▾ ├ └ │ ↻ → ·`) are all single UTF-16 units, so a UTF-16 count
//! matches Qt exactly. Internally the buffer is a [`Utf16Buf`]; the final
//! [`ComposeResult::text`] is converted to a `String` once at the end.

use std::collections::{HashMap, HashSet};

use crate::core::linemeta::{ChipKind, K_COMMAND_ROW_ID};
use crate::core::{
    is_hex_node, is_hex_preview, is_valid_primitive_ptr_target, kind_meta, lines_for_kind,
    size_for_kind, ComposeResult, LayoutInfo, LineChip, LineKind, LineMeta, Node, NodeKind,
    NodeTree,
};
use crate::provider::{ModuleEntry, NullProvider, Provider};
#[cfg(feature = "symbols")]
use crate::rtti::walk::{walk_rtti, walk_rtti_itanium, RttiInfo};

// When the `symbols` feature is disabled the RTTI walker (`crate::rtti`) is not
// compiled. The composition engine is always-on (it does not depend on heavy
// deps), so we supply a feature-stubbed `RttiInfo` whose `ok` is permanently
// `false`. Every RTTI auto-detect call site stays identical; without `symbols`
// the hint simply never fires, which is the correct headless behavior.
#[cfg(not(feature = "symbols"))]
#[derive(Clone, Default)]
struct RttiInfo {
    ok: bool,
    demangled_name: String,
}

// ───────────────────────────────────────────────────────────────────────────
// Column layout constants (`core.h:1129-1148`). Re-declared here because the
// composition engine OWNS the column geometry (ARCHITECTURE.md §3) and the
// existing `core::linemeta` exposes the same constants; we mirror the values
// 1:1 so callers can use either set interchangeably.
// ───────────────────────────────────────────────────────────────────────────

/// 3-char fold indicator prefix per line (`core.h:1130`).
pub const K_FOLD_COL: i32 = 3;
/// chars per nesting-level indent (ReClass `core.h:1131` uses 2; widened to 3 for
/// clearer nested indentation — kept in sync with the `linemeta`/`format` copies).
pub const K_TREE_INDENT: i32 = 3;
/// Max type column width (`core.h:1132`).
pub const K_COL_TYPE: i32 = 14;
pub const K_COL_NAME: i32 = 22;
pub const K_COL_VALUE: i32 = 96;
pub const K_COL_COMMENT: i32 = 28;
pub const K_COL_BASE_ADDR: i32 = 12;
pub const K_SEP_WIDTH: i32 = 1;
pub const K_MIN_TYPE_W: i32 = 9;
pub const K_MAX_TYPE_W: i32 = 128;
pub const K_MIN_NAME_W: i32 = 10;
pub const K_MAX_NAME_W: i32 = 128;
pub const K_COMPACT_TYPE_W: i32 = 20;
pub const K_DEFAULT_REFRESH_MS: i32 = 200;

// Scintilla fold constants (`compose.cpp:67-69`).
const SC_FOLDLEVELBASE: i32 = 0x400;
const SC_FOLDLEVELHEADERFLAG: i32 = 0x2000;
const GOLDEN_RATIO: u64 = 0x9E37_79B9_7F4A_7C15;

// Marker bit indices (`core.h:182-193`) — duplicated locally for the markerMask
// math (the same values are in `core::linemeta`).
const M_CONT: u32 = 0;
const M_CYCLE: u32 = 3;
const M_ERR: u32 = 4;
const M_STRUCT_BG: u32 = 5;

// ───────────────────────────────────────────────────────────────────────────
// `Utf16Buf` — a UTF-16 code-unit buffer (mirrors Qt `QString` for column math).
// ───────────────────────────────────────────────────────────────────────────

/// A growable buffer of UTF-16 code units. All compose column arithmetic indexes
/// in UTF-16 units to match Qt's `QString` exactly (`compose.cpp` map §A.5).
#[derive(Default)]
struct Utf16Buf {
    units: Vec<u16>,
}

impl Utf16Buf {
    fn with_capacity(n: usize) -> Self {
        Utf16Buf {
            units: Vec::with_capacity(n),
        }
    }
    /// Number of UTF-16 code units (Qt `QString::size()`).
    fn len(&self) -> usize {
        self.units.len()
    }
    fn push_char(&mut self, c: char) {
        let mut b = [0u16; 2];
        for u in c.encode_utf16(&mut b) {
            self.units.push(*u);
        }
    }
    fn push_str16(&mut self, s: &U16Str) {
        self.units.extend_from_slice(&s.units);
    }
    /// Code unit at `i` (0 if out of range).
    fn unit_at(&self, i: usize) -> u16 {
        self.units.get(i).copied().unwrap_or(0)
    }
    fn to_string(&self) -> String {
        String::from_utf16_lossy(&self.units)
    }
}

/// An owned UTF-16 string built from a Rust `&str`, used so all per-line column
/// math is in UTF-16 units. (Qt `QString` analogue for line construction.)
#[derive(Clone, Default)]
struct U16Str {
    units: Vec<u16>,
}

impl U16Str {
    fn new() -> Self {
        U16Str { units: Vec::new() }
    }
    fn from_str(s: &str) -> Self {
        U16Str {
            units: s.encode_utf16().collect(),
        }
    }
    fn len(&self) -> usize {
        self.units.len()
    }
    fn push_str(&mut self, s: &str) {
        self.units.extend(s.encode_utf16());
    }
    fn push_u16str(&mut self, s: &U16Str) {
        self.units.extend_from_slice(&s.units);
    }
    fn unit_at(&self, i: usize) -> u16 {
        self.units.get(i).copied().unwrap_or(0)
    }
    fn ends_with_unit(&self, u: u16) -> bool {
        self.units.last() == Some(&u)
    }
    fn chop(&mut self) {
        self.units.pop();
    }
}

const SP: u16 = b' ' as u16;
const TAB: u16 = b'\t' as u16;
const LBRACE: u16 = b'{' as u16;

// ───────────────────────────────────────────────────────────────────────────
// Compose options + entry points.
// ───────────────────────────────────────────────────────────────────────────

/// Optional PDB symbol-lookup callback (`SymbolLookupFn`, `core.h:1474`). `None`
/// in headless builds.
pub type SymbolLookupFn<'a> = Option<Box<dyn Fn(u64) -> String + 'a>>;

/// `rcx::compose(...)` (decl `core.h:1529-1534`, defined `compose.cpp:1510`).
///
/// Renders `tree` against the live `provider` into a [`ComposeResult`]. The
/// trailing booleans mirror the C++ default arguments. Use [`compose_default`]
/// for the common 2-argument call.
#[allow(clippy::too_many_arguments)]
pub fn compose(
    tree: &NodeTree,
    provider: &dyn Provider,
    view_root_id: u64,
    compact_columns: bool,
    tree_lines: bool,
    brace_wrap: bool,
    type_hints: bool,
    show_comments: bool,
    show_rtti: bool,
    show_enum_chips: bool,
) -> ComposeResult {
    compose_with_symbols(
        tree,
        provider,
        view_root_id,
        compact_columns,
        tree_lines,
        brace_wrap,
        type_hints,
        show_comments,
        None,
        show_rtti,
        show_enum_chips,
    )
}

/// Thin helper matching the many C++ test calls `compose(tree, prov)` (all
/// defaults: `viewRootId=0, compactColumns=false, treeLines=false,
/// braceWrap=false, typeHints=false, showComments=true, showRtti=true,
/// showEnumChips=true`).
pub fn compose_default(tree: &NodeTree, provider: &dyn Provider) -> ComposeResult {
    compose(
        tree, provider, 0, false, false, false, false, true, true, true,
    )
}

/// Full entry point with the optional symbol-lookup closure (the last positional
/// C++ argument, between `showComments` and `showRtti`).
#[allow(clippy::too_many_arguments)]
pub fn compose_with_symbols(
    tree: &NodeTree,
    provider: &dyn Provider,
    view_root_id: u64,
    compact_columns: bool,
    tree_lines: bool,
    brace_wrap: bool,
    type_hints: bool,
    show_comments: bool,
    symbol_lookup: SymbolLookupFn<'_>,
    show_rtti: bool,
    show_enum_chips: bool,
) -> ComposeResult {
    let mut state = ComposeState {
        text: Utf16Buf::with_capacity(tree.nodes.len() * 80),
        meta: Vec::with_capacity(tree.nodes.len() * 3),
        line_starts: Vec::new(),
        max_line_len: 0,
        visiting: HashSet::new(),
        ptr_visiting: HashSet::new(),
        virtual_ptr_refs: HashSet::new(),
        current_line: 0,
        type_w: K_COL_TYPE,
        name_w: K_COL_NAME,
        offset_hex_digits: 8,
        base_emitted: false,
        compact_columns,
        tree_lines,
        brace_wrap,
        type_hints,
        show_comments,
        show_rtti,
        show_enum_chips,
        symbol_lookup,
        sibling_stack: Vec::new(),
        current_ptr_base: 0,
        child_map: HashMap::new(),
        child_map_sorted: HashSet::new(),
        abs_offsets: Vec::new(),
        scope_type_w: HashMap::new(),
        scope_name_w: HashMap::new(),
        rtti_modules_cached: false,
        rtti_modules: Vec::new(),
        rtti_cache: HashMap::new(),
    };

    // Precompute parent→children map (`compose.cpp:1527-1528`).
    for (i, n) in tree.nodes.iter().enumerate() {
        state
            .child_map
            .entry(n.parent_id)
            .or_default()
            .push(i as i32);
    }

    // Absolute-offset BFS (`compose.cpp:1539-1586`).
    {
        let n = tree.nodes.len();
        state.abs_offsets = vec![0i64; n];
        let mut visited = vec![false; n];
        for (i, node) in tree.nodes.iter().enumerate() {
            if node.parent_id == 0 {
                state.abs_offsets[i] = node.offset as i64;
            }
        }
        let mut bfs_queue: Vec<i32> = Vec::new();
        if let Some(roots) = state.child_map.get(&0) {
            for &i in roots {
                bfs_queue.push(i);
                visited[i as usize] = true;
            }
        }
        let mut front = 0usize;
        while front < bfs_queue.len() {
            let idx = bfs_queue[front] as usize;
            front += 1;
            let pi = tree.index_of_id(tree.nodes[idx].parent_id);
            state.abs_offsets[idx] = (if pi >= 0 {
                state.abs_offsets[pi as usize]
            } else {
                0
            }) + tree.nodes[idx].offset as i64;
            if let Some(kids) = state.child_map.get(&tree.nodes[idx].id) {
                for &ci in kids {
                    if !visited[ci as usize] {
                        visited[ci as usize] = true;
                        bfs_queue.push(ci);
                    }
                }
            }
        }
        // Re-root unvisited orphans at their own offset, then DFS-fix descendants.
        for i in 0..n {
            if !visited[i] {
                state.abs_offsets[i] = tree.nodes[i].offset as i64;
                visited[i] = true;
                let mut stack = vec![i];
                while let Some(p) = stack.pop() {
                    if let Some(kids) = state.child_map.get(&tree.nodes[p].id) {
                        let kids: Vec<i32> = kids.clone();
                        for ci in kids {
                            let ci = ci as usize;
                            if visited[ci] {
                                continue;
                            }
                            visited[ci] = true;
                            state.abs_offsets[ci] =
                                state.abs_offsets[p] + tree.nodes[ci].offset as i64;
                            stack.push(ci);
                        }
                    }
                }
            }
        }
        for v in state.abs_offsets.iter_mut() {
            *v = v.wrapping_add(tree.base_address as i64);
        }
    }

    // Hex-digit tier (`compose.cpp:1589-1599`).
    {
        let mut max_addr = tree.base_address;
        for &a in &state.abs_offsets {
            let addr = a as u64;
            if addr > max_addr {
                max_addr = addr;
            }
        }
        state.offset_hex_digits = if max_addr <= 0xFFFF {
            4
        } else if max_addr <= 0xFFFF_FFFF {
            8
        } else if max_addr <= 0xFFFF_FFFF_FFFF {
            12
        } else {
            16
        };
    }

    // Column widths (`compose.cpp:1601-1688`).
    {
        let type_name_len = |n: &Node| -> i32 { node_type_name(tree, n).len() as i32 };

        // Pre-compute type-name lengths.
        let type_name_lens: Vec<i32> = tree.nodes.iter().map(type_name_len).collect();

        let type_cap = if state.compact_columns {
            K_COMPACT_TYPE_W
        } else {
            K_MAX_TYPE_W
        };
        let mut max_type_len = K_MIN_TYPE_W;
        let mut max_name_len = K_MIN_NAME_W;
        for (i, node) in tree.nodes.iter().enumerate() {
            max_type_len = max_type_len.max(type_name_lens[i]);
            max_name_len = max_name_len.max(u16_len(&node.name));
        }
        state.type_w = max_type_len.clamp(K_MIN_TYPE_W, type_cap);
        state.name_w = max_name_len.clamp(K_MIN_NAME_W, K_MAX_NAME_W);

        // Per-scope widths (per container, direct non-struct children only).
        for (_i, container) in tree.nodes.iter().enumerate() {
            if container.kind != NodeKind::Struct && container.kind != NodeKind::Array {
                continue;
            }
            let mut scope_max_type = K_MIN_TYPE_W;
            let mut scope_max_name = K_MIN_NAME_W;
            let empty = Vec::new();
            let kids = state.child_map.get(&container.id).unwrap_or(&empty);
            for &child_idx in kids {
                let child = &tree.nodes[child_idx as usize];
                if child.kind == NodeKind::Struct {
                    continue; // pointer headers shouldn't inflate sibling widths
                }
                scope_max_type = scope_max_type.max(type_name_lens[child_idx as usize]);
                scope_max_name = scope_max_name.max(u16_len(&child.name));
            }
            // Primitive arrays with no tree children: account for synthesized
            // element type names ("uint32_t[99]").
            if container.kind == NodeKind::Array
                && kids.is_empty()
                && container.element_kind != NodeKind::Struct
                && container.element_kind != NodeKind::Array
                && container.array_len > 0
            {
                let max_idx = container.array_len - 1;
                let longest = format!(
                    "{}[{}]",
                    render::type_name_raw(container.element_kind),
                    max_idx
                );
                scope_max_type = scope_max_type.max(u16_len(&longest));
            }
            state
                .scope_type_w
                .insert(container.id, scope_max_type.clamp(K_MIN_TYPE_W, type_cap));
            state.scope_name_w.insert(
                container.id,
                scope_max_name.clamp(K_MIN_NAME_W, K_MAX_NAME_W),
            );
        }

        // Root level (parentId == 0).
        {
            let mut root_max_type = K_MIN_TYPE_W;
            let mut root_max_name = K_MIN_NAME_W;
            let empty = Vec::new();
            let kids = state.child_map.get(&0).unwrap_or(&empty);
            for &child_idx in kids {
                let child = &tree.nodes[child_idx as usize];
                if child.kind == NodeKind::Struct {
                    continue;
                }
                root_max_type = root_max_type.max(type_name_lens[child_idx as usize]);
                root_max_name = root_max_name.max(u16_len(&child.name));
            }
            state
                .scope_type_w
                .insert(0, root_max_type.clamp(K_MIN_TYPE_W, type_cap));
            state
                .scope_name_w
                .insert(0, root_max_name.clamp(K_MIN_NAME_W, K_MAX_NAME_W));
        }
    }

    // Emit CommandRow as line 0 (`compose.cpp:1690-1710`).
    {
        let cmd_row_text = "[\u{25B8}] source\u{25BE}  0x0  struct Untitled {";
        let mut lm = LineMeta {
            node_idx: -1,
            node_id: K_COMMAND_ROW_ID,
            depth: 0,
            line_kind: LineKind::CommandRow,
            fold_level: SC_FOLDLEVELBASE,
            fold_head: false,
            offset_text: render::fmt_offset_margin(
                tree.base_address,
                false,
                state.offset_hex_digits,
            ),
            offset_addr: tree.base_address,
            ptr_base: state.current_ptr_base,
            marker_mask: 0,
            effective_type_w: state.type_w,
            effective_name_w: state.name_w,
            ..Default::default()
        };
        let t = U16Str::from_str(cmd_row_text);
        state.emit_line(&t, &mut lm);
    }

    // Brace wrapping: standalone "{" after CommandRow (`compose.cpp:1712-1724`).
    if state.brace_wrap {
        let mut brace_lm = LineMeta {
            node_idx: -1,
            node_id: 0,
            depth: 0,
            line_kind: LineKind::Footer,
            is_root_header: true,
            fold_level: SC_FOLDLEVELBASE,
            marker_mask: 0,
            ..Default::default()
        };
        let t = U16Str::from_str("{");
        state.emit_line(&t, &mut brace_lm);
    }

    // Walk roots (`compose.cpp:1726-1736`).
    let roots = state.child_indices(0).to_vec();
    for idx in roots {
        if view_root_id != 0 && tree.nodes[idx as usize].id != view_root_id {
            continue;
        }
        compose_node(&mut state, tree, provider, idx, 0, 0, 0, false, 0, -1, 0);
    }

    ComposeResult {
        text: state.text.to_string(),
        meta: state.meta,
        layout: LayoutInfo {
            type_w: state.type_w,
            name_w: state.name_w,
            offset_hex_digits: state.offset_hex_digits,
            base_address: tree.base_address,
            tree_lines,
        },
        max_line_len: state.max_line_len,
        line_starts: state.line_starts.iter().map(|&v| v as i32).collect(),
    }
}

#[inline]
fn u16_len(s: &str) -> i32 {
    s.encode_utf16().count() as i32
}

// ───────────────────────────────────────────────────────────────────────────
// libstdc++ `std::sort` (introsort) — faithful port so the lazy child sort in
// `childIndices` (`compose.cpp:252`) produces the EXACT same ordering of
// equal-offset siblings as the C++ (`std::sort` is unstable; Rust's `sort` is
// stable, which would diverge on the golden EPROCESS root ordering).
// Sorts `v` (node indices) ascending by `abs[v[i]]`.
// ───────────────────────────────────────────────────────────────────────────

const SORT_THRESHOLD: usize = 16;

#[inline]
fn lg(mut n: usize) -> i32 {
    // floor(log2(n)) for n >= 1, matching libstdc++ `std::__lg`.
    let mut k = 0i32;
    while n > 1 {
        n >>= 1;
        k += 1;
    }
    k
}

fn std_sort_by_abs(v: &mut [i32], abs: &[i64]) {
    if v.is_empty() {
        return;
    }
    let key = |x: i32| abs[x as usize];
    let n = v.len();
    introsort_loop(v, 0, n, lg(n) * 2, &key);
    final_insertion_sort(v, 0, n, &key);
}

fn move_median_to_first<K: Fn(i32) -> i64>(
    v: &mut [i32],
    result: usize,
    a: usize,
    b: usize,
    c: usize,
    key: &K,
) {
    let lt = |x: usize, y: usize| key(v[x]) < key(v[y]);
    if lt(a, b) {
        if lt(b, c) {
            v.swap(result, b);
        } else if lt(a, c) {
            v.swap(result, c);
        } else {
            v.swap(result, a);
        }
    } else if lt(a, c) {
        v.swap(result, a);
    } else if lt(b, c) {
        v.swap(result, c);
    } else {
        v.swap(result, b);
    }
}

fn unguarded_partition<K: Fn(i32) -> i64>(
    v: &mut [i32],
    mut first: usize,
    mut last: usize,
    pivot: usize,
    key: &K,
) -> usize {
    loop {
        while key(v[first]) < key(v[pivot]) {
            first += 1;
        }
        last -= 1;
        while key(v[pivot]) < key(v[last]) {
            last -= 1;
        }
        if first >= last {
            return first;
        }
        v.swap(first, last);
        first += 1;
    }
}

fn unguarded_partition_pivot<K: Fn(i32) -> i64>(
    v: &mut [i32],
    first: usize,
    last: usize,
    key: &K,
) -> usize {
    let mid = first + (last - first) / 2;
    move_median_to_first(v, first, first + 1, mid, last - 1, key);
    unguarded_partition(v, first + 1, last, first, key)
}

fn introsort_loop<K: Fn(i32) -> i64>(
    v: &mut [i32],
    first: usize,
    mut last: usize,
    mut depth: i32,
    key: &K,
) {
    while last - first > SORT_THRESHOLD {
        if depth == 0 {
            // libstdc++ falls back to heapsort (`partial_sort`) here; with the
            // shallow trees compose produces this depth limit is never hit, and
            // any compose tree small enough not to recurse this deep matches.
            heap_sort(v, first, last, key);
            return;
        }
        depth -= 1;
        let cut = unguarded_partition_pivot(v, first, last, key);
        introsort_loop(v, cut, last, depth, key);
        last = cut;
    }
}

fn insertion_sort<K: Fn(i32) -> i64>(v: &mut [i32], first: usize, last: usize, key: &K) {
    if first == last {
        return;
    }
    for i in (first + 1)..last {
        if key(v[i]) < key(v[first]) {
            let val = v[i];
            let mut j = i;
            while j > first {
                v[j] = v[j - 1];
                j -= 1;
            }
            v[first] = val;
        } else {
            let val = v[i];
            let mut j = i;
            while key(val) < key(v[j - 1]) {
                v[j] = v[j - 1];
                j -= 1;
            }
            v[j] = val;
        }
    }
}

fn unguarded_insertion_sort<K: Fn(i32) -> i64>(v: &mut [i32], first: usize, last: usize, key: &K) {
    for i in first..last {
        let val = v[i];
        let mut j = i;
        while j > 0 && key(val) < key(v[j - 1]) {
            v[j] = v[j - 1];
            j -= 1;
        }
        v[j] = val;
    }
}

fn final_insertion_sort<K: Fn(i32) -> i64>(v: &mut [i32], first: usize, last: usize, key: &K) {
    if last - first > SORT_THRESHOLD {
        insertion_sort(v, first, first + SORT_THRESHOLD, key);
        unguarded_insertion_sort(v, first + SORT_THRESHOLD, last, key);
    } else {
        insertion_sort(v, first, last, key);
    }
}

// libstdc++ `std::__heap_select`/`partial_sort` fallback (depth-limit reached).
// A straightforward heapsort over [first,last) by `key` keeps parity for the
// (rare) deeply-recursive case.
fn heap_sort<K: Fn(i32) -> i64>(v: &mut [i32], first: usize, last: usize, key: &K) {
    let n = last - first;
    if n < 2 {
        return;
    }
    // build max-heap
    for start in (0..n / 2).rev() {
        sift_down(v, first, start, n, key);
    }
    for end in (1..n).rev() {
        v.swap(first, first + end);
        sift_down(v, first, 0, end, key);
    }
}

fn sift_down<K: Fn(i32) -> i64>(v: &mut [i32], first: usize, mut root: usize, n: usize, key: &K) {
    loop {
        let mut largest = root;
        let l = 2 * root + 1;
        let r = 2 * root + 2;
        if l < n && key(v[first + l]) > key(v[first + largest]) {
            largest = l;
        }
        if r < n && key(v[first + r]) > key(v[first + largest]) {
            largest = r;
        }
        if largest == root {
            break;
        }
        v.swap(first + root, first + largest);
        root = largest;
    }
}

// ───────────────────────────────────────────────────────────────────────────
// ComposeState — mutable accumulator threaded through the recursion.
// ───────────────────────────────────────────────────────────────────────────

struct ComposeState<'a> {
    text: Utf16Buf,
    meta: Vec<LineMeta>,
    line_starts: Vec<usize>,
    max_line_len: i32,
    visiting: HashSet<u64>,
    ptr_visiting: HashSet<u64>,
    virtual_ptr_refs: HashSet<u64>,
    current_line: i32,
    type_w: i32,
    name_w: i32,
    offset_hex_digits: i32,
    base_emitted: bool,
    compact_columns: bool,
    tree_lines: bool,
    brace_wrap: bool,
    #[allow(dead_code)]
    type_hints: bool,
    show_comments: bool,
    show_rtti: bool,
    show_enum_chips: bool,
    symbol_lookup: SymbolLookupFn<'a>,
    sibling_stack: Vec<bool>,
    current_ptr_base: u64,
    child_map: HashMap<u64, Vec<i32>>,
    child_map_sorted: HashSet<u64>,
    abs_offsets: Vec<i64>,
    scope_type_w: HashMap<u64, i32>,
    scope_name_w: HashMap<u64, i32>,

    // ── RTTI auto-detect cache (per compose pass) ──
    // Module list is fetched lazily on the first vtable candidate. `walk_rtti`
    // results are memoized — both successes (avoid re-walk) and failures
    // (avoid re-trying every refresh on the same arbitrary 8-byte word).
    // (`compose.cpp:85-91`.)
    rtti_modules_cached: bool,
    rtti_modules: Vec<ModuleEntry>,
    rtti_cache: HashMap<u64, RttiInfo>,
}

impl ComposeState<'_> {
    fn effective_type_w(&self, scope_id: u64) -> i32 {
        *self.scope_type_w.get(&scope_id).unwrap_or(&self.type_w)
    }
    fn effective_name_w(&self, scope_id: u64) -> i32 {
        *self.scope_name_w.get(&scope_id).unwrap_or(&self.name_w)
    }

    /// `setTreeSibling(childDepth, hasMoreSiblings)` (`compose.cpp:121-126`).
    fn set_tree_sibling(&mut self, child_depth: i32, has_more: bool) {
        if !self.tree_lines {
            return;
        }
        let d = child_depth - 1;
        if d < 0 {
            return;
        }
        while (self.sibling_stack.len() as i32) <= d {
            self.sibling_stack.push(false);
        }
        self.sibling_stack[d as usize] = has_more;
    }

    /// `childIndices(state, parentId)` (`compose.cpp:245-258`) — lazily sorts
    /// children by absolute offset on first access (stable sort).
    fn child_indices(&mut self, parent_id: u64) -> &[i32] {
        if !self.child_map.contains_key(&parent_id) {
            return &[];
        }
        if !self.child_map_sorted.contains(&parent_id) {
            // Sort a detached copy against abs_offsets, then write back.
            // **Fidelity:** the C++ uses `std::sort` (`compose.cpp:252`), which
            // is UNSTABLE; the byte-exact golden EPROCESS dumps depend on its
            // exact ordering of equal-offset roots. Rust's `slice::sort` is
            // stable and would reorder them differently, so we reproduce
            // libstdc++'s introsort (`std_sort_by_abs`) verbatim.
            let mut children = self.child_map.remove(&parent_id).unwrap();
            std_sort_by_abs(&mut children, &self.abs_offsets);
            self.child_map.insert(parent_id, children);
            self.child_map_sorted.insert(parent_id);
        }
        self.child_map
            .get(&parent_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// `emitLine(lineText, LineMeta&&)` (`compose.cpp:128-193`).
    fn emit_line(&mut self, line_text: &U16Str, lm: &mut LineMeta) {
        if self.current_line > 0 {
            self.text.push_char('\n');
        }
        self.line_starts.push(self.text.len());
        let line_start_units = self.text.len();

        // 3-char fold indicator prefix.
        if lm.line_kind == LineKind::CommandRow
            || (lm.line_kind == LineKind::Footer && lm.is_root_header)
        {
            // no prefix — flush left
        } else if lm.fold_head {
            if lm.fold_collapsed {
                self.text.push_str16(&U16Str::from_str(" \u{25B8} "));
            } else {
                self.text.push_str16(&U16Str::from_str(" \u{25BE} "));
            }
        } else {
            self.text.push_str16(&U16Str::from_str("   "));
        }

        // Replace leading indent spaces with Unicode tree connectors. Each level
        // occupies `K_TREE_INDENT` columns: the connector glyph (`│`/`├`/`└`, or a
        // space for an inactive ancestor) followed by `K_TREE_INDENT - 1` spaces, so
        // the connectors scale with the indent width and stay aligned with the
        // `K_TREE_INDENT`-spaced indent the rest of the layout assumes.
        if self.tree_lines && lm.depth > 0 {
            let mut tree_indent = U16Str::new();
            let big_d = lm.depth;
            let is_footer = lm.line_kind == LineKind::Footer;
            let pad = " ".repeat((K_TREE_INDENT - 1).max(0) as usize);
            for d in 0..big_d {
                let active =
                    (d as usize) < self.sibling_stack.len() && self.sibling_stack[d as usize];
                let glyph = if is_footer || d < big_d - 1 {
                    if active {
                        "\u{2502}"
                    } else {
                        " "
                    }
                } else if active {
                    "\u{251C}"
                } else {
                    "\u{2514}"
                };
                tree_indent.push_str(glyph);
                tree_indent.push_str(&pad);
            }
            self.text.push_str16(&tree_indent);
            // lineText.mid(D * kTreeIndent) — slice off the leading indent.
            let skip = (big_d * K_TREE_INDENT) as usize;
            for i in skip..line_text.len() {
                self.text.units.push(line_text.unit_at(i));
            }
        } else {
            self.text.push_str16(line_text);
        }

        // Auto-detect trailing '{' for brace_col.
        if lm.brace_col < 0
            && (lm.line_kind == LineKind::Header || lm.line_kind == LineKind::CommandRow)
        {
            let geom = LineGeometry::for_line(lm);
            let len = line_text.len();
            let mut p = len as i64 - 1;
            while p >= 0 {
                let ch = line_text.unit_at(p as usize);
                if ch == SP || ch == TAB {
                    p -= 1;
                    continue;
                }
                if ch == LBRACE {
                    lm.brace_col = geom.document_column(p as i32);
                }
                break;
            }
        }

        self.meta.push(lm.clone());
        self.current_line += 1;

        // Track longest line (trailing spaces excluded).
        let line_end = self.text.len();
        let len = line_end - line_start_units;
        let mut last_non_space = len;
        while last_non_space > 0 && self.text.unit_at(line_start_units + last_non_space - 1) == SP {
            last_non_space -= 1;
        }
        if last_non_space as i32 > self.max_line_len {
            self.max_line_len = last_non_space as i32;
        }
    }
}

#[inline]
fn compute_fold_level(depth: i32, is_head: bool) -> i32 {
    let mut level = SC_FOLDLEVELBASE + depth;
    if is_head {
        level |= SC_FOLDLEVELHEADERFLAG;
    }
    level
}

#[inline]
fn compute_markers(is_cont: bool) -> u32 {
    let mut mask = 0u32;
    if is_cont {
        mask |= 1u32 << M_CONT;
    }
    mask
}

/// `resolvePointerTarget` (`compose.cpp:210-216`).
fn resolve_pointer_target(tree: &NodeTree, ref_id: u64) -> String {
    if ref_id == 0 {
        return String::new();
    }
    let ref_idx = tree.index_of_id(ref_id);
    if ref_idx < 0 {
        return String::new();
    }
    let r = &tree.nodes[ref_idx as usize];
    if r.struct_type_name.is_empty() {
        r.name.clone()
    } else {
        r.struct_type_name.clone()
    }
}

/// `relOffsetFromRoot` (`compose.cpp:218-233`).
fn rel_offset_from_root(tree: &NodeTree, idx: i32, root_id: u64) -> i64 {
    let mut total: i64 = 0;
    let mut visited: HashSet<u64> = HashSet::new();
    let mut cur = idx;
    while cur >= 0 && (cur as usize) < tree.nodes.len() {
        let nid = tree.nodes[cur as usize].id;
        if visited.contains(&nid) {
            break;
        }
        visited.insert(nid);
        let n = &tree.nodes[cur as usize];
        if n.id == root_id {
            break;
        }
        total += n.offset as i64;
        if n.parent_id == 0 {
            break;
        }
        cur = tree.index_of_id(n.parent_id);
    }
    total
}

/// `resolveAddr` (`compose.cpp:235-242`).
fn resolve_addr(
    state: &ComposeState,
    tree: &NodeTree,
    node_idx: i32,
    base: u64,
    root_id: u64,
) -> u64 {
    if root_id != 0 {
        return base.wrapping_add(rel_offset_from_root(tree, node_idx, root_id) as u64);
    }
    state.abs_offsets[node_idx as usize] as u64
}

/// Resolve RTTI for a candidate vtable address, cached per compose pass
/// (`compose.cpp:250-286`).
///
/// Module enumeration runs at most once per pass; values that don't land inside
/// any known module short-circuit before [`walk_rtti`] is even called. Negative
/// results (`ok=false`) are cached too — this prevents the parser from being
/// re-run on the same arbitrary qword every refresh tick.
///
/// `max_vtable_slots=0` is honored by the walker's slot loop (`rtti.cpp:236`) —
/// it yields the demangled class name without enumerating method addresses,
/// which is all the inline hint needs.
///
/// Returns a clone of the cached [`RttiInfo`] (cheap; the hint only reads `ok` +
/// `demangled_name`). The C++ returns a `const &` into the cache, but Rust's
/// borrow rules make a clone the simplest faithful equivalent.
fn rtti_for_vtable(state: &mut ComposeState, prov: &dyn Provider, candidate_addr: u64) -> RttiInfo {
    if let Some(info) = state.rtti_cache.get(&candidate_addr) {
        return info.clone();
    }

    if !state.rtti_modules_cached {
        state.rtti_modules = prov.enumerate_modules();
        state.rtti_modules_cached = true;
    }

    // ok=false default — caches negative results. (`mut` is only exercised when
    // the `symbols` feature is on and the RTTI walker can overwrite `info`.)
    #[cfg_attr(not(feature = "symbols"), allow(unused_mut))]
    let mut info = RttiInfo::default();
    let mut in_module = false;
    for m in &state.rtti_modules {
        if candidate_addr >= m.base && candidate_addr < m.base.wrapping_add(m.size) {
            in_module = true;
            break;
        }
    }
    #[cfg(feature = "symbols")]
    if in_module {
        // Try MSVC RTTI first (signature-validated, lower false-positive risk).
        // Fall back to Itanium ABI for GCC/Clang/MinGW binaries.
        info = walk_rtti(prov, candidate_addr, 8, 0);
        if !info.ok {
            info = walk_rtti_itanium(prov, candidate_addr, 8, 0);
        }
    }
    // `symbols` off: `in_module` is computed but the walker is unavailable, so
    // `info` stays at its `ok=false` default (no RTTI hint emitted).
    let _ = in_module;
    state.rtti_cache.insert(candidate_addr, info.clone());
    info
}

/// `node`-display type string (the `nodeTypeName` lambda, `compose.cpp:1602-1613`).
fn node_type_name(tree: &NodeTree, n: &Node) -> String {
    match n.kind {
        NodeKind::Array => {
            let sn = if n.element_kind == NodeKind::Struct {
                resolve_pointer_target(tree, n.ref_id)
            } else {
                String::new()
            };
            render::array_type_name(n.element_kind, n.array_len, &sn)
        }
        NodeKind::Struct => render::struct_type_name(n),
        NodeKind::Pointer32 | NodeKind::Pointer64 => {
            render::pointer_type_name(resolve_pointer_target(tree, n.ref_id))
        }
        _ => render::type_name_raw(n.kind),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Value preview for type hints (`compose.cpp:18-56` — the file-local
// `formatPreview`). Formats raw bytes as the suggested type using the `fmt::`
// formatters. Drives the TypeHint chip's value-first text
// (`preview + " [" + typeName + "]"`, `compose.cpp:450-458`). Recursive: a
// multi-lane suggestion splits the bytes into `kinds.len()` equal slices and
// joins each lane's preview with ", ".
// ───────────────────────────────────────────────────────────────────────────

fn format_preview(data: &[u8], len: i32, kinds: &[NodeKind]) -> String {
    use crate::format as fmt;

    let Some(&k) = kinds.first() else {
        return String::new();
    };

    // Native-byte-order loads, matching the C++ `detail::loadU*/loadF*`
    // (`typeinfer.h:48-61`), i.e. little-endian on x86_64.
    let load_u16 = |d: &[u8]| -> u16 { u16::from_le_bytes([d[0], d[1]]) };
    let load_u32 = |d: &[u8]| -> u32 { u32::from_le_bytes([d[0], d[1], d[2], d[3]]) };
    let load_u64 =
        |d: &[u8]| -> u64 { u64::from_le_bytes([d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]) };
    let load_f32 = |d: &[u8]| -> f32 { f32::from_bits(load_u32(d)) };
    let load_f64 = |d: &[u8]| -> f64 { f64::from_bits(load_u64(d)) };

    if kinds.len() == 1 {
        return match k {
            NodeKind::Float => fmt::fmt_float(load_f32(data)),
            NodeKind::Double => fmt::fmt_double(load_f64(data)),
            NodeKind::Int32 => fmt::fmt_int32(load_u32(data) as i32),
            NodeKind::UInt32 => fmt::fmt_uint32(load_u32(data)),
            NodeKind::Int16 => fmt::fmt_int16(load_u16(data) as i16),
            NodeKind::UInt16 => fmt::fmt_uint16(load_u16(data)),
            NodeKind::Int64 => fmt::fmt_int64(load_u64(data) as i64),
            NodeKind::UInt64 => fmt::fmt_uint64(load_u64(data)),
            NodeKind::Pointer64 => fmt::fmt_pointer64(load_u64(data)),
            NodeKind::Pointer32 => fmt::fmt_pointer32(load_u32(data)),
            NodeKind::Bool => fmt::fmt_bool(data[0]),
            NodeKind::UTF8 => {
                let n = len.min(8).max(0) as usize;
                let mut s = String::new();
                for &c in data.iter().take(n) {
                    if (0x20..=0x7E).contains(&c) {
                        s.push(c as char);
                    } else {
                        break;
                    }
                }
                if s.is_empty() {
                    String::new()
                } else {
                    format!("\"{s}\"")
                }
            }
            _ => String::new(),
        };
    }

    // Split: show each part (uniform split into `kinds.len()` lanes).
    let part_sz = len / kinds.len() as i32;
    let part_sz_usize = part_sz.max(0) as usize;
    let parts: Vec<String> = kinds
        .iter()
        .enumerate()
        .map(|(i, &lane)| {
            let start = i * part_sz_usize;
            let slice = data.get(start..).unwrap_or(&[]);
            format_preview(slice, part_sz, std::slice::from_ref(&lane))
        })
        .collect();
    parts.join(", ")
}

// ───────────────────────────────────────────────────────────────────────────
// Chip sanitize helper (`compose.cpp:394-404`).
// ───────────────────────────────────────────────────────────────────────────

fn sanitize_chip(s: &str) -> String {
    if s.contains('\n') || s.contains('\r') || s.contains('\t') {
        let mut out = s
            .replace('\r', " ")
            .replace('\t', " ")
            .replace('\n', " \u{00B7} ");
        // Squash runs of 2+ spaces to a single space ("  +" → " ").
        while out.contains("  ") {
            out = out.replace("  ", " ");
        }
        out
    } else {
        s.to_string()
    }
}

// ───────────────────────────────────────────────────────────────────────────
// composeLeaf (`compose.cpp:305-593`).
// ───────────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn compose_leaf(
    state: &mut ComposeState,
    tree: &NodeTree,
    prov: &dyn Provider,
    node_idx: i32,
    depth: i32,
    abs_addr: u64,
    scope_id: u64,
) {
    let node = tree.nodes[node_idx as usize].clone();

    let mut parent_abs_addr = 0u64;
    if depth > 0 {
        let pi = tree.index_of_id(node.parent_id);
        if pi >= 0 && (pi as usize) < state.abs_offsets.len() {
            parent_abs_addr = state.abs_offsets[pi as usize] as u64;
        }
    }

    let type_w = state.effective_type_w(scope_id);
    let name_w = state.effective_name_w(scope_id);

    let num_lines = lines_for_kind(node.kind);

    // Resolve pointer target name + type override.
    let mut ptr_type_override = String::new();
    let mut ptr_target_name = String::new();
    if node.kind == NodeKind::Pointer32 || node.kind == NodeKind::Pointer64 {
        if node.ptr_depth > 0
            && node.ref_id == 0
            && is_valid_primitive_ptr_target(node.element_kind)
        {
            let base_name = kind_meta(node.element_kind)
                .map(|m| m.type_name.to_string())
                .unwrap_or_else(|| "void".to_string());
            let stars = if node.ptr_depth >= 2 { "**" } else { "*" };
            ptr_type_override = format!("{base_name}{stars}");
        } else {
            ptr_target_name = resolve_pointer_target(tree, node.ref_id);
            let stars: String = "*".repeat((node.ptr_depth + 1) as usize);
            let base = if ptr_target_name.is_empty() {
                "void"
            } else {
                &ptr_target_name
            };
            ptr_type_override = format!("{base}{stars}");
        }
    }

    let raw_type = if ptr_type_override.is_empty() {
        render::type_name_raw(node.kind)
    } else {
        ptr_type_override.clone()
    };
    let type_overflow = state.compact_columns && u16_len(&raw_type) > type_w;
    let line_type_w = if type_overflow {
        u16_len(&raw_type)
    } else {
        type_w
    };

    for sub in 0..num_lines {
        let is_cont = sub > 0;

        let mut lm = LineMeta {
            node_idx,
            node_id: node.id,
            sub_line: sub,
            depth,
            is_continuation: is_cont,
            line_kind: if is_cont {
                LineKind::Continuation
            } else {
                LineKind::Field
            },
            node_kind: node.kind,
            offset_text: render::fmt_offset_margin(abs_addr, is_cont, state.offset_hex_digits),
            offset_addr: abs_addr,
            ptr_base: state.current_ptr_base,
            parent_addr: parent_abs_addr,
            marker_mask: compute_markers(is_cont),
            fold_level: compute_fold_level(depth, false),
            effective_type_w: line_type_w,
            effective_name_w: name_w,
            pointer_target_name: ptr_target_name.clone(),
            ..Default::default()
        };

        if is_hex_preview(node.kind) {
            lm.line_byte_count = size_for_kind(node.kind);
        }

        let mut line_text = render::fmt_node_line(
            &node,
            prov,
            abs_addr,
            depth,
            sub,
            "",
            type_w,
            name_w,
            &ptr_type_override,
            state.compact_columns,
        );

        // ── Chip block (sub == 0): Enum, [TypeHint], Rtti, Comment ──
        if sub == 0 {
            // 1. Enum chip.
            if state.show_enum_chips
                && node.ref_id != 0
                && matches!(
                    node.kind,
                    NodeKind::UInt8
                        | NodeKind::UInt16
                        | NodeKind::UInt32
                        | NodeKind::UInt64
                        | NodeKind::Int8
                        | NodeKind::Int16
                        | NodeKind::Int32
                        | NodeKind::Int64
                )
                && prov.is_readable(abs_addr, node.byte_size())
            {
                let ref_idx = tree.index_of_id(node.ref_id);
                if ref_idx >= 0 {
                    let ref_node = &tree.nodes[ref_idx as usize];
                    if ref_node.is_enum() && !ref_node.enum_members.is_empty() {
                        let v: i64 = match node.kind {
                            NodeKind::UInt8 => prov.read_u8(abs_addr) as i64,
                            NodeKind::UInt16 => prov.read_u16(abs_addr) as i64,
                            NodeKind::UInt32 => prov.read_u32(abs_addr) as i64,
                            NodeKind::UInt64 => prov.read_u64(abs_addr) as i64,
                            NodeKind::Int8 => (prov.read_u8(abs_addr) as i8) as i64,
                            NodeKind::Int16 => (prov.read_u16(abs_addr) as i16) as i64,
                            NodeKind::Int32 => (prov.read_u32(abs_addr) as i32) as i64,
                            NodeKind::Int64 => prov.read_u64(abs_addr) as i64,
                            _ => 0,
                        };
                        let member_name = ref_node
                            .enum_members
                            .iter()
                            .find(|(_, val)| *val == v)
                            .map(|(name, _)| name.clone());
                        if let Some(name) = member_name {
                            if !name.is_empty() {
                                let chip_text = format!("({name})");
                                let ref_id = node.ref_id;
                                push_chip(
                                    &mut line_text,
                                    &mut lm,
                                    ChipKind::Enum,
                                    &chip_text,
                                    |c| {
                                        c.enum_current_value = v;
                                        c.enum_ref_node_id = ref_id;
                                    },
                                );
                            }
                        }
                    }
                }
            }

            // 2b. Comment chip. Mirrors `composeLeaf` (`compose.cpp:401-438`)
            //    order: the comment annotation is appended to the line text
            //    immediately AFTER the enum member name and BEFORE the type-hint
            //    and RTTI annotations. Chip order on the line therefore is
            //    enum -> comment -> typeHint -> RTTI, which fixes the trailing
            //    text order and every chip's start/end col.
            if state.show_comments {
                let mut comment_text = String::new();
                if !node.comment.is_empty() {
                    comment_text = node.comment.clone();
                } else if let Some(lookup) = state.symbol_lookup.as_ref() {
                    let sym = lookup(abs_addr);
                    if !sym.is_empty() {
                        comment_text = sym;
                    }
                }
                if !comment_text.is_empty() {
                    // Prefix the chip text with the literal "// " lead-in to
                    // match the authoritative C++ displayed string: `compose.cpp:436`
                    // appends `"  // " + commentText` to the line text, so the
                    // row literally shows `  // IHDR …`. `push_chip` prepends the
                    // `"  "` separator (after trimming value-column padding), so a
                    // chip text of `"// IHDR"` reproduces `"  // IHDR"` byte-for-byte.
                    // The `// ` is added here (not via a fill closure) so the
                    // start_col/end_col span and `sanitize_chip` (collapses embedded
                    // \r\n\t to keep the chip on one row) cover the prefixed string.
                    let chip_text = format!("// {comment_text}");
                    push_chip(
                        &mut line_text,
                        &mut lm,
                        ChipKind::Comment,
                        &chip_text,
                        |_| {},
                    );
                }
            }

            // 3. TypeHint — type-inference annotation on hex preview nodes.
            // Gated on `state.type_hints` (`compose.cpp:441`); with the flag off
            // the green chip is suppressed (`test_rtti_hint.cpp:313`).
            if state.type_hints && is_hex_node(node.kind) {
                let sz = size_for_kind(node.kind);
                let b = if prov.is_readable(abs_addr, sz) {
                    prov.read_bytes(abs_addr, sz)
                } else {
                    vec![0u8; sz as usize]
                };
                // `infer_types` returns empty for all-zero / empty input (the
                // degenerate inputs the C++ also skips). For non-zero data the
                // scoring pipeline lives in the `typeinfer` workflow; guard so
                // headless compose never trips its skeleton.
                if b.iter().any(|&x| x != 0) {
                    let suggestions = crate::core::infer_types(&b, &Default::default(), 3);
                    if let Some(first) = suggestions.first() {
                        if first.strength >= 3 {
                            // Value-preview + bracketed type label, mirroring
                            // `lm.typeHint` (`compose.cpp:450-458`):
                            //   "0x7ff718570000 [ptr64]"  /  "-99999+f, -0.0000f [Float×2]"
                            // When the preview is empty fall back to "[type]"
                            // (`compose.cpp:457`).
                            let type_name = crate::core::format_hint(first);
                            let preview = format_preview(&b, sz, &first.kinds);
                            let chip_text = if preview.is_empty() {
                                format!("[{type_name}]")
                            } else {
                                format!("{preview} [{type_name}]")
                            };
                            let kinds = first.kinds.clone();
                            push_chip(
                                &mut line_text,
                                &mut lm,
                                ChipKind::TypeHint,
                                &chip_text,
                                |c| {
                                    c.type_hint_kinds = kinds;
                                },
                            );
                        }
                    }
                }
            }

            // 4. RTTI auto-detect — Hex64/Pointer64 whose value lands inside a
            //    known module is a vtable candidate. The module-range scan
            //    rejects ~99% of values cheaply; surviving candidates run
            //    `walk_rtti` once and the result is cached for the rest of this
            //    compose pass (`rtti_for_vtable`). Independent of `type_hints`
            //    and `show_comments` — RTTI is "real signal" worth showing on
            //    its own (`compose.cpp:462-484`, `test_rtti_hint.cpp:312`).
            //    Appended LAST (after enum/comment/typeHint), matching C++
            //    `composeLeaf` order. The null-pointer CTA chip (port-specific)
            //    is gated on `show_rtti`; the PDB symbol annotation now rides in
            //    the value text via `read_value` (`format.cpp:425`), so no
            //    Symbol chip.
            if (node.kind == NodeKind::Hex64 || node.kind == NodeKind::Pointer64)
                && prov.is_readable(abs_addr, 8)
            {
                let candidate = prov.read_u64(abs_addr);
                if candidate == 0
                    && state.show_rtti
                    && (node.kind == NodeKind::Pointer64 || node.kind == NodeKind::Pointer32)
                {
                    push_chip(
                        &mut line_text,
                        &mut lm,
                        ChipKind::Rtti,
                        "(Name class\u{2026})",
                        |c| {
                            c.rtti_vtable_addr = 0;
                        },
                    );
                } else if candidate != 0 && candidate != u64::MAX {
                    let info = rtti_for_vtable(state, prov, candidate);
                    if info.ok && !info.demangled_name.is_empty() {
                        let hint = format!("{{RTTI: {}}}", info.demangled_name);
                        push_chip(&mut line_text, &mut lm, ChipKind::Rtti, &hint, |c| {
                            c.rtti_vtable_addr = candidate;
                        });
                    }
                }
            }
        }

        state.emit_line(&line_text, &mut lm);
    }
}

/// `pushChip` (`compose.cpp:405-423`) — trims trailing value-column padding,
/// appends `"  " + sanitize(text)` to `line_text`, records `[startCol,endCol)`
/// in document-column space.
fn push_chip<F: FnOnce(&mut LineChip)>(
    line_text: &mut U16Str,
    lm: &mut LineMeta,
    kind: ChipKind,
    raw_text: &str,
    fill: F,
) {
    let text = sanitize_chip(raw_text);
    let mut c = LineChip {
        kind,
        text: text.clone(),
        ..Default::default()
    };
    while line_text.ends_with_unit(SP) {
        line_text.chop();
    }
    line_text.push_str("  ");
    let text16 = U16Str::from_str(&text);
    line_text.push_u16str(&text16);
    let geom = LineGeometry::for_line(lm);
    c.start_col = geom.document_column((line_text.len() - text16.len()) as i32);
    c.end_col = geom.document_column(line_text.len() as i32);
    fill(&mut c);
    lm.chips.push(c);
}

// ───────────────────────────────────────────────────────────────────────────
// composeParent (`compose.cpp:607-1232`).
// ───────────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn compose_parent(
    state: &mut ComposeState,
    tree: &NodeTree,
    prov: &dyn Provider,
    node_idx: i32,
    depth: i32,
    base: u64,
    root_id: u64,
    is_array_child: bool,
    scope_id: u64,
    array_element_idx: i32,
    array_container_addr: u64,
) {
    let node = tree.nodes[node_idx as usize].clone();
    let abs_addr = resolve_addr(state, tree, node_idx, base, root_id);

    // Cycle detection.
    if state.visiting.contains(&node.id) {
        let mut lm = LineMeta {
            node_idx,
            node_id: node.id,
            depth,
            line_kind: LineKind::Field,
            offset_text: render::fmt_offset_margin(abs_addr, false, state.offset_hex_digits),
            offset_addr: abs_addr,
            ptr_base: state.current_ptr_base,
            node_kind: node.kind,
            marker_mask: (1u32 << M_CYCLE) | (1u32 << M_ERR),
            fold_level: compute_fold_level(depth, false),
            ..Default::default()
        };
        let mut t = U16Str::from_str(&render::indent(depth));
        t.push_str("\u{21BB} ");
        t.push_str(&node.name);
        t.push_str("  (circular reference)");
        state.emit_line(&t, &mut lm);
        return;
    }
    state.visiting.insert(node.id);

    // Array element separator.
    if is_array_child && array_element_idx >= 0 {
        let rel_off = abs_addr.wrapping_sub(array_container_addr);
        let rel_off_hex = format!("{rel_off:X}");
        let mut lm = LineMeta {
            node_idx,
            node_id: node.id,
            depth,
            line_kind: LineKind::ArrayElementSeparator,
            offset_text: render::fmt_offset_margin(abs_addr, false, state.offset_hex_digits),
            offset_addr: abs_addr,
            ptr_base: state.current_ptr_base,
            node_kind: node.kind,
            fold_level: compute_fold_level(depth, false),
            marker_mask: 1u32 << M_STRUCT_BG,
            array_element_idx,
            ..Default::default()
        };
        let mut t = U16Str::from_str(&render::indent(depth));
        t.push_str(&format!("[{array_element_idx}] +0x{rel_off_hex}"));
        state.emit_line(&t, &mut lm);
    }

    // Root header detection (first root-level struct, suppressed from display).
    let is_root_header =
        node.parent_id == 0 && node.kind == NodeKind::Struct && !state.base_emitted;
    if is_root_header {
        state.base_emitted = true;
    }

    // Header line.
    if !is_array_child && !is_root_header {
        let type_w = state.effective_type_w(scope_id);
        let name_w = state.effective_name_w(scope_id);

        let mut lm = LineMeta {
            node_idx,
            node_id: node.id,
            depth,
            line_kind: LineKind::Header,
            offset_text: render::fmt_offset_margin(abs_addr, false, state.offset_hex_digits),
            offset_addr: abs_addr,
            ptr_base: state.current_ptr_base,
            node_kind: node.kind,
            is_root_header: false,
            fold_head: true,
            fold_collapsed: node.collapsed,
            fold_level: compute_fold_level(depth, true),
            marker_mask: 1u32 << M_STRUCT_BG,
            ..Default::default()
        };

        let mut header_text;
        if node.kind == NodeKind::Array {
            lm.is_array_header = true;
            lm.element_kind = node.element_kind;
            lm.array_view_idx = node.view_index;
            lm.array_count = node.array_len;
            let elem_struct_name = if node.element_kind == NodeKind::Struct {
                resolve_pointer_target(tree, node.ref_id)
            } else {
                String::new()
            };
            let raw_type =
                render::array_type_name(node.element_kind, node.array_len, &elem_struct_name);
            let overflow = state.compact_columns && u16_len(&raw_type) > type_w;
            lm.effective_type_w = if overflow { u16_len(&raw_type) } else { type_w };
            lm.effective_name_w = name_w;
            header_text = render::fmt_array_header(
                &node,
                depth,
                node.collapsed,
                type_w,
                name_w,
                &elem_struct_name,
                state.compact_columns,
            );
        } else {
            let raw_type = render::struct_type_name(&node);
            let overflow = state.compact_columns && u16_len(&raw_type) > type_w;
            lm.effective_type_w = if overflow { u16_len(&raw_type) } else { type_w };
            lm.effective_name_w = name_w;
            header_text = render::fmt_struct_header(
                &node,
                depth,
                node.collapsed,
                type_w,
                name_w,
                state.compact_columns,
            );
        }

        // Brace wrapping.
        if state.brace_wrap && !node.collapsed && header_text.ends_with_unit(LBRACE) {
            header_text.chop();
            while header_text.ends_with_unit(SP) {
                header_text.chop();
            }
            state.emit_line(&header_text, &mut lm);
            let mut brace_lm = LineMeta {
                node_idx,
                node_id: node.id,
                depth,
                line_kind: LineKind::Footer,
                fold_level: compute_fold_level(depth, true),
                marker_mask: 1u32 << M_STRUCT_BG,
                ..Default::default()
            };
            let mut t = U16Str::from_str(&render::indent(depth));
            t.push_str("{");
            state.emit_line(&t, &mut brace_lm);
        } else {
            state.emit_line(&header_text, &mut lm);
        }
    }

    if !node.collapsed || is_array_child || is_root_header {
        // Enum members.
        if node.is_enum() && !node.enum_members.is_empty() {
            let child_depth = depth + 1;
            let mut max_name_len = 4i32;
            for (name, _) in &node.enum_members {
                max_name_len = max_name_len.max(u16_len(name));
            }
            // Display order sorted by value. C++ uses `std::sort` (introsort,
            // unstable: `compose.cpp:629`), so route through the libstdc++-faithful
            // introsort helper keyed on `enum_members[idx].value` to reproduce the
            // exact tie order on equal values (Vec::sort_by is stable and would
            // diverge).
            let mut order: Vec<i32> = (0..node.enum_members.len() as i32).collect();
            let enum_values: Vec<i64> = node.enum_members.iter().map(|(_, v)| *v).collect();
            std_sort_by_abs(&mut order, &enum_values);
            let order: Vec<usize> = order.into_iter().map(|x| x as usize).collect();

            for (oi, &mi) in order.iter().enumerate() {
                state.set_tree_sibling(child_depth, oi < order.len() - 1);
                let (mname, mval) = &node.enum_members[mi];
                let mut lm = LineMeta {
                    node_idx,
                    node_id: node.id,
                    sub_line: mi as i32,
                    depth: child_depth,
                    line_kind: LineKind::Field,
                    is_member_line: true,
                    node_kind: NodeKind::UInt32,
                    fold_level: compute_fold_level(child_depth, false),
                    marker_mask: 0,
                    offset_text: render::fmt_offset_margin(abs_addr, true, state.offset_hex_digits),
                    offset_addr: abs_addr,
                    ptr_base: state.current_ptr_base,
                    ..Default::default()
                };
                let t = render::fmt_enum_member(mname, *mval, child_depth, max_name_len);
                state.emit_line(&t, &mut lm);
            }

            if !is_array_child {
                let mut lm = LineMeta {
                    node_idx,
                    node_id: node.id,
                    depth,
                    line_kind: LineKind::Footer,
                    node_kind: node.kind,
                    is_root_header,
                    fold_level: compute_fold_level(depth, false),
                    marker_mask: 0,
                    offset_text: render::fmt_offset_margin(
                        abs_addr,
                        false,
                        state.offset_hex_digits,
                    ),
                    offset_addr: abs_addr,
                    ptr_base: state.current_ptr_base,
                    ..Default::default()
                };
                let t = render::fmt_struct_footer(&node, depth, 0);
                state.emit_line(&t, &mut lm);
            }
            state.visiting.remove(&node.id);
            return;
        }

        // Bitfield members.
        if node.is_bitfield() && !node.bitfield_members.is_empty() {
            let child_depth = depth + 1;
            let mut max_name_len = 4i32;
            for m in &node.bitfield_members {
                max_name_len = max_name_len.max(u16_len(&m.name));
            }
            let count = node.bitfield_members.len();
            for mi in 0..count {
                state.set_tree_sibling(child_depth, mi < count - 1);
                let m = &node.bitfield_members[mi];
                let bit_val = render::extract_bits(
                    prov,
                    abs_addr,
                    node.element_kind,
                    m.bit_offset,
                    m.bit_width,
                );
                let mut lm = LineMeta {
                    node_idx,
                    node_id: node.id,
                    sub_line: mi as i32,
                    depth: child_depth,
                    line_kind: LineKind::Field,
                    is_member_line: true,
                    node_kind: node.element_kind,
                    fold_level: compute_fold_level(child_depth, false),
                    marker_mask: 0,
                    offset_text: render::fmt_offset_margin(abs_addr, true, state.offset_hex_digits),
                    offset_addr: abs_addr,
                    ptr_base: state.current_ptr_base,
                    ..Default::default()
                };
                let t = render::fmt_bitfield_member(
                    &m.name,
                    m.bit_width,
                    bit_val,
                    child_depth,
                    max_name_len,
                );
                state.emit_line(&t, &mut lm);
            }

            if !is_array_child {
                let sz = size_for_kind(node.element_kind);
                let mut lm = LineMeta {
                    node_idx,
                    node_id: node.id,
                    depth,
                    line_kind: LineKind::Footer,
                    node_kind: node.kind,
                    is_root_header,
                    fold_level: compute_fold_level(depth, false),
                    marker_mask: 0,
                    offset_text: render::fmt_offset_margin(
                        abs_addr.wrapping_add(sz as u64),
                        false,
                        state.offset_hex_digits,
                    ),
                    offset_addr: abs_addr.wrapping_add(sz as u64),
                    ptr_base: state.current_ptr_base,
                    ..Default::default()
                };
                let t = render::fmt_struct_footer(&node, depth, sz);
                state.emit_line(&t, &mut lm);
            }
            state.visiting.remove(&node.id);
            return;
        }

        let all_children: Vec<i32> = state.child_indices(node.id).to_vec();

        // Split regular vs static.
        let mut regular: Vec<i32> = Vec::new();
        let mut static_idxs: Vec<i32> = Vec::new();
        for &ci in &all_children {
            if tree.nodes[ci as usize].is_static {
                static_idxs.push(ci);
            } else {
                regular.push(ci);
            }
        }

        let child_depth = depth + 1;

        // Primitive arrays with no child nodes: synthesize element lines.
        if node.kind == NodeKind::Array
            && regular.is_empty()
            && node.element_kind != NodeKind::Struct
            && node.element_kind != NodeKind::Array
        {
            let elem_size = size_for_kind(node.element_kind);
            let e_tw = state.effective_type_w(node.id);
            let e_nw = state.effective_name_w(node.id);
            for i in 0..node.array_len {
                state.set_tree_sibling(child_depth, i < node.array_len - 1);
                let elem_addr = abs_addr.wrapping_add(i as u64 * elem_size as u64);
                let elem_type_str = format!("{}[{}]", render::type_name_raw(node.element_kind), i);

                let elem = Node {
                    kind: node.element_kind,
                    name: String::new(),
                    offset: node.offset + (i as u64 * elem_size as u64) as i32,
                    parent_id: node.id,
                    id: 0,
                    ..Node::default()
                };

                let elem_overflow = state.compact_columns && u16_len(&elem_type_str) > e_tw;
                let mut lm = LineMeta {
                    node_idx,
                    node_id: node.id,
                    depth: child_depth,
                    line_kind: LineKind::Field,
                    node_kind: node.element_kind,
                    is_array_element: true,
                    parent_addr: abs_addr,
                    array_element_idx: i,
                    offset_text: render::fmt_offset_margin(
                        elem_addr,
                        false,
                        state.offset_hex_digits,
                    ),
                    offset_addr: elem_addr,
                    ptr_base: state.current_ptr_base,
                    marker_mask: compute_markers(false),
                    fold_level: compute_fold_level(child_depth, false),
                    effective_type_w: if elem_overflow {
                        u16_len(&elem_type_str)
                    } else {
                        e_tw
                    },
                    effective_name_w: e_nw,
                    ..Default::default()
                };
                let t = render::fmt_node_line(
                    &elem,
                    prov,
                    elem_addr,
                    child_depth,
                    0,
                    "",
                    e_tw,
                    e_nw,
                    &elem_type_str,
                    state.compact_columns,
                );
                state.emit_line(&t, &mut lm);
            }
        }

        // Struct arrays with refId but no child nodes.
        if node.kind == NodeKind::Array
            && regular.is_empty()
            && node.element_kind == NodeKind::Struct
            && node.ref_id != 0
        {
            let ref_idx = tree.index_of_id(node.ref_id);
            if ref_idx >= 0 {
                let mut elem_size = tree.struct_span(node.ref_id);
                if elem_size <= 0 {
                    elem_size = 1;
                }
                for i in 0..node.array_len {
                    state.set_tree_sibling(child_depth, i < node.array_len - 1);
                    let elem_base = abs_addr.wrapping_add(i as u64 * elem_size as u64);
                    compose_parent(
                        state,
                        tree,
                        prov,
                        ref_idx,
                        child_depth,
                        elem_base,
                        node.ref_id,
                        true,
                        node.id,
                        i,
                        abs_addr,
                    );
                }
            }
        }

        // Embedded struct with refId but no child nodes.
        if node.kind == NodeKind::Struct && regular.is_empty() && node.ref_id != 0 {
            let ref_idx = tree.index_of_id(node.ref_id);
            if ref_idx >= 0 {
                let ref_children: Vec<i32> = state.child_indices(node.ref_id).to_vec();
                let ref_scope_id = node.ref_id;
                let n_ref = ref_children.len();
                for (rci, &child_idx) in ref_children.iter().enumerate() {
                    state.set_tree_sibling(child_depth, rci < n_ref - 1);
                    let child = tree.nodes[child_idx as usize].clone();
                    if state.visiting.contains(&child.id) {
                        let type_w = state.effective_type_w(ref_scope_id);
                        let name_w = state.effective_name_w(ref_scope_id);
                        let raw_type = render::struct_type_name(&child);
                        let overflow = state.compact_columns && u16_len(&raw_type) > type_w;
                        let mut lm = LineMeta {
                            node_idx,
                            node_id: child.id,
                            depth: child_depth,
                            line_kind: LineKind::Header,
                            offset_text: render::fmt_offset_margin(
                                abs_addr.wrapping_add(child.offset as u64),
                                false,
                                state.offset_hex_digits,
                            ),
                            offset_addr: abs_addr.wrapping_add(child.offset as u64),
                            ptr_base: state.current_ptr_base,
                            node_kind: child.kind,
                            fold_head: true,
                            fold_collapsed: true,
                            fold_level: compute_fold_level(child_depth, true),
                            marker_mask: (1u32 << M_STRUCT_BG) | (1u32 << M_CYCLE),
                            effective_type_w: if overflow { u16_len(&raw_type) } else { type_w },
                            effective_name_w: name_w,
                            ..Default::default()
                        };
                        let t = render::fmt_struct_header(
                            &child,
                            child_depth,
                            true,
                            type_w,
                            name_w,
                            state.compact_columns,
                        );
                        state.emit_line(&t, &mut lm);
                        continue;
                    }
                    compose_node(
                        state,
                        tree,
                        prov,
                        child_idx,
                        child_depth,
                        abs_addr,
                        node.ref_id,
                        false,
                        ref_scope_id,
                        -1,
                        0,
                    );
                }
            }
        }

        // Regular children.
        let children_are_array_elements = node.kind == NodeKind::Array;
        let mut element_idx = 0;
        // Gap-ruler tracking (`compose.cpp:850-893`): accumulate consecutive
        // unnamed (padding) children's byte sizes; just before the next NAMED
        // child, emit a non-interactive "[+0xN gap]" continuation line, then
        // reset. Structs/arrays contribute their struct_span, leaves their
        // byteSize. Hex is lowercase to match Qt `.arg(gapBytes, 0, 16)`.
        let mut gap_bytes: i32 = 0;
        let n_reg = regular.len();
        for ri in 0..n_reg {
            let child_idx = regular[ri];
            let child = &tree.nodes[child_idx as usize];

            // Emit gap-ruler line just before a named field that follows padding.
            if !children_are_array_elements && !child.name.is_empty() && gap_bytes > 0 {
                let gap_start_addr = abs_addr
                    .wrapping_add(child.offset as u64)
                    .wrapping_sub(gap_bytes as u64);
                let mut gm = LineMeta {
                    node_idx: -1, // non-interactive
                    node_id: 0,
                    depth: child_depth,
                    line_kind: LineKind::Continuation,
                    is_continuation: true,
                    parent_addr: abs_addr,
                    fold_level: compute_fold_level(child_depth, false),
                    offset_text: render::fmt_offset_margin(
                        gap_start_addr,
                        false,
                        state.offset_hex_digits,
                    ),
                    offset_addr: gap_start_addr,
                    ..Default::default()
                };
                let mut body = U16Str::from_str(&render::indent(child_depth));
                body.push_str(&format!("[+0x{gap_bytes:x} gap]"));
                state.emit_line(&body, &mut gm);
            }

            let child_name_empty = child.name.is_empty();
            let child_kind = child.kind;
            let child_id = child.id;
            let child_byte_size = child.byte_size();

            let has_more = (ri < n_reg - 1) || (!static_idxs.is_empty() && !node.collapsed);
            state.set_tree_sibling(child_depth, has_more);
            let (elem_idx_arg, container_addr_arg) = if children_are_array_elements {
                let e = element_idx;
                element_idx += 1;
                (e, abs_addr)
            } else {
                (-1, 0)
            };
            compose_node(
                state,
                tree,
                prov,
                child_idx,
                child_depth,
                base,
                root_id,
                children_are_array_elements,
                node.id,
                elem_idx_arg,
                container_addr_arg,
            );

            // Update gap accumulation: empty-name children contribute, named ones reset.
            if !children_are_array_elements {
                if child_name_empty {
                    let sz = if child_kind == NodeKind::Struct || child_kind == NodeKind::Array {
                        tree.struct_span(child_id)
                    } else {
                        child_byte_size
                    };
                    gap_bytes += sz;
                } else {
                    gap_bytes = 0;
                }
            }
        }

        // ── Static fields ──
        if !static_idxs.is_empty() && (!node.collapsed || is_root_header) {
            compose_static_fields(
                state,
                tree,
                prov,
                &node,
                abs_addr,
                child_depth,
                &regular,
                &static_idxs,
            );
        }
    }

    // Footer line.
    if !is_array_child && (!node.collapsed || is_root_header) {
        let sz = tree.struct_span(node.id);
        let mut lm = LineMeta {
            node_idx,
            node_id: node.id,
            depth,
            line_kind: LineKind::Footer,
            node_kind: node.kind,
            is_root_header,
            fold_level: compute_fold_level(depth, false),
            marker_mask: 0,
            offset_text: render::fmt_offset_margin(
                abs_addr.wrapping_add(sz as u64),
                false,
                state.offset_hex_digits,
            ),
            offset_addr: abs_addr.wrapping_add(sz as u64),
            ptr_base: state.current_ptr_base,
            ..Default::default()
        };
        let t = render::fmt_struct_footer(&node, depth, sz);
        state.emit_line(&t, &mut lm);
    }

    state.visiting.remove(&node.id);
}

/// Static-field rendering block (`compose.cpp:970-1210`). Split out for clarity.
#[allow(clippy::too_many_arguments)]
fn compose_static_fields(
    state: &mut ComposeState,
    tree: &NodeTree,
    prov: &dyn Provider,
    _parent: &Node,
    abs_addr: u64,
    child_depth: i32,
    regular: &[i32],
    static_idxs: &[i32],
) {
    use crate::addr::{AddressParser, AddressParserCallbacks};

    // Build the resolver mirroring compose.cpp's makeResolver.
    let make_callbacks = |parent_abs_addr: u64| -> AddressParserCallbacks<'_> {
        let ps = tree.pointer_size;
        AddressParserCallbacks {
            resolve_identifier: Some(Box::new(move |name: &str| -> (u64, bool) {
                if name == "base" {
                    return (parent_abs_addr, true);
                }
                for &ci in regular {
                    let sib = &tree.nodes[ci as usize];
                    if sib.name == name {
                        let sz = sib.byte_size();
                        let sib_addr = parent_abs_addr.wrapping_add(sib.offset as u64);
                        if sz > 0 && prov.is_valid() && prov.is_readable(sib_addr, sz) {
                            let v = match sz {
                                1 => prov.read_u8(sib_addr) as u64,
                                2 => prov.read_u16(sib_addr) as u64,
                                4 => prov.read_u32(sib_addr) as u64,
                                _ => prov.read_u64(sib_addr),
                            };
                            return (v, true);
                        }
                        return (0, false);
                    }
                }
                (0, false)
            })),
            read_pointer: Some(Box::new(move |addr: u64| -> (u64, bool) {
                if prov.is_valid() && prov.is_readable(addr, ps) {
                    let v = if ps >= 8 {
                        prov.read_u64(addr)
                    } else {
                        prov.read_u32(addr) as u64
                    };
                    (v, true)
                } else {
                    (0, false)
                }
            })),
            resolve_module: Some(Box::new(move |name: &str| -> (u64, bool) {
                let base = prov.symbol_to_address(name);
                (base, base != 0)
            })),
            ..Default::default()
        }
    };

    let cbs = make_callbacks(abs_addr);

    let n_static = static_idxs.len();
    for sii in 0..n_static {
        let si = static_idxs[sii];
        state.set_tree_sibling(child_depth, sii < n_static - 1);
        let sf = tree.nodes[si as usize].clone();

        // Evaluate expression → absolute address.
        let mut static_addr = 0u64;
        let mut expr_ok = false;
        if !sf.offset_expr.is_empty() {
            let result = AddressParser::evaluate(&sf.offset_expr, tree.pointer_size, Some(&cbs));
            expr_ok = result.ok;
            if result.ok {
                static_addr = result.value;
            }
        }

        // Resolve type name.
        let type_name = if sf.kind == NodeKind::Struct {
            render::struct_type_name(&sf)
        } else if sf.kind == NodeKind::Pointer64 || sf.kind == NodeKind::Pointer32 {
            render::pointer_type_name_kind(resolve_pointer_target(tree, sf.ref_id))
        } else {
            render::type_name_raw(sf.kind)
        };

        let is_collapsed = sf.collapsed;

        // Header line.
        let mut header_line = U16Str::from_str(&render::indent(child_depth));
        if is_collapsed {
            let expr_part = if !sf.offset_expr.is_empty() {
                if expr_ok {
                    format!("return {} }} \u{2192} 0x{:X}", sf.offset_expr, static_addr)
                } else {
                    format!("return {} }}  (error)", sf.offset_expr)
                }
            } else {
                "}".to_string()
            };
            header_line.push_str(&format!(
                "static {} {} {{ {}",
                type_name, sf.name, expr_part
            ));
        } else {
            header_line.push_str(&format!("static {} {} {{", type_name, sf.name));
        }

        let offset_text = format!(
            "~{}",
            zero_pad_hex_upper(static_addr, (state.offset_hex_digits - 1) as usize)
        );
        let mut lm = LineMeta {
            node_idx: si,
            node_id: sf.id,
            depth: child_depth,
            line_kind: LineKind::Header,
            node_kind: sf.kind,
            fold_head: true,
            fold_collapsed: is_collapsed,
            is_static_line: true,
            fold_level: compute_fold_level(child_depth, true),
            marker_mask: 1u32 << M_STRUCT_BG,
            offset_text,
            offset_addr: static_addr,
            ptr_base: state.current_ptr_base,
            effective_type_w: u16_len(&type_name) + 7,
            effective_name_w: u16_len(&sf.name),
            ..Default::default()
        };
        state.emit_line(&header_line, &mut lm);

        // Body + children (only when expanded).
        if !is_collapsed {
            let mut has_struct_kids =
                expr_ok && (sf.kind == NodeKind::Struct || sf.kind == NodeKind::Array);
            let static_kids: Vec<i32> = if has_struct_kids {
                state.child_indices(sf.id).to_vec()
            } else {
                Vec::new()
            };
            has_struct_kids = has_struct_kids && !static_kids.is_empty();

            // Body line.
            {
                state.set_tree_sibling(child_depth + 1, has_struct_kids);
                let mut body_line = U16Str::from_str(&render::indent(child_depth + 1));
                if !sf.offset_expr.is_empty() {
                    if expr_ok {
                        body_line.push_str(&format!("return {}", sf.offset_expr));
                    } else {
                        body_line.push_str(&format!("return {}  (error)", sf.offset_expr));
                    }
                } else {
                    body_line.push_str("return 0");
                }
                if expr_ok && !sf.offset_expr.is_empty() {
                    body_line.push_str(&format!("  \u{2192} 0x{static_addr:X}"));
                }

                let mut blm = LineMeta {
                    node_idx: si,
                    node_id: sf.id,
                    depth: child_depth + 1,
                    line_kind: LineKind::Field,
                    node_kind: sf.kind,
                    is_static_line: true,
                    fold_level: compute_fold_level(child_depth + 1, false),
                    marker_mask: 0,
                    offset_text: " ".repeat(state.offset_hex_digits as usize),
                    offset_addr: static_addr,
                    ptr_base: state.current_ptr_base,
                    ..Default::default()
                };
                state.emit_line(&body_line, &mut blm);
            }

            // Struct/array children at evaluated address.
            if has_struct_kids {
                let n_kids = static_kids.len();
                for ski in 0..n_kids {
                    state.set_tree_sibling(child_depth + 1, ski < n_kids - 1);
                    compose_node(
                        state,
                        tree,
                        prov,
                        static_kids[ski],
                        child_depth + 1,
                        static_addr,
                        sf.id,
                        false,
                        sf.id,
                        -1,
                        0,
                    );
                }
            }

            // Static pointer: read pointer value, expand ref struct.
            if expr_ok
                && sf.ref_id != 0
                && (sf.kind == NodeKind::Pointer64 || sf.kind == NodeKind::Pointer32)
            {
                let psz = sf.byte_size();
                let mut ptr_val = 0u64;
                if prov.is_valid() && psz > 0 && prov.is_readable(static_addr, psz) {
                    ptr_val = if sf.kind == NodeKind::Pointer32 {
                        prov.read_u32(static_addr) as u64
                    } else {
                        prov.read_u64(static_addr)
                    };
                    if ptr_val == u64::MAX
                        || (sf.kind == NodeKind::Pointer32 && ptr_val == 0xFFFF_FFFF)
                    {
                        ptr_val = 0;
                    }
                }
                if sf.is_relative && ptr_val != 0 {
                    ptr_val = ptr_val.wrapping_add(abs_addr);
                }
                if ptr_val != 0 {
                    let mut p_base = ptr_val;
                    let ptr_readable = prov.is_readable(p_base, 1);
                    if !ptr_readable {
                        p_base = 0;
                    }
                    let null_prov = NullProvider;
                    let child_prov: &dyn Provider = if ptr_readable { prov } else { &null_prov };

                    let ref_idx = tree.index_of_id(sf.ref_id);
                    if ref_idx >= 0 {
                        let ref_node = &tree.nodes[ref_idx as usize];
                        if ref_node.kind == NodeKind::Struct || ref_node.kind == NodeKind::Array {
                            let ref_id = ref_node.id;
                            let saved = state.current_ptr_base;
                            state.current_ptr_base = p_base;
                            compose_parent(
                                state,
                                tree,
                                child_prov,
                                ref_idx,
                                child_depth,
                                p_base,
                                ref_id,
                                true,
                                0,
                                -1,
                                0,
                            );
                            state.current_ptr_base = saved;
                        }
                    }
                }
            }

            // Footer line "};".
            {
                let (offset_text, offset_addr) =
                    if expr_ok && (sf.kind == NodeKind::Struct || sf.kind == NodeKind::Array) {
                        let s_span = tree.struct_span(sf.id);
                        (
                            render::fmt_offset_margin(
                                static_addr.wrapping_add(s_span as u64),
                                false,
                                state.offset_hex_digits,
                            ),
                            static_addr.wrapping_add(s_span as u64),
                        )
                    } else {
                        (" ".repeat(state.offset_hex_digits as usize), static_addr)
                    };
                let mut flm = LineMeta {
                    node_idx: si,
                    node_id: sf.id,
                    depth: child_depth,
                    line_kind: LineKind::Footer,
                    node_kind: sf.kind,
                    is_static_line: true,
                    fold_level: compute_fold_level(child_depth, false),
                    marker_mask: 0,
                    offset_text,
                    offset_addr,
                    ptr_base: state.current_ptr_base,
                    ..Default::default()
                };
                let mut t = U16Str::from_str(&render::indent(child_depth));
                t.push_str("};");
                state.emit_line(&t, &mut flm);
            }
        }
    }
}

/// `QString::number(v,16).toUpper().rightJustified(digits,'0')`.
fn zero_pad_hex_upper(v: u64, digits: usize) -> String {
    let s = format!("{v:X}");
    if s.len() >= digits {
        s
    } else {
        format!("{}{}", "0".repeat(digits - s.len()), s)
    }
}

// ───────────────────────────────────────────────────────────────────────────
// composeNode (`compose.cpp:1234-1506`).
// ───────────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn compose_node(
    state: &mut ComposeState,
    tree: &NodeTree,
    prov: &dyn Provider,
    node_idx: i32,
    depth: i32,
    base: u64,
    root_id: u64,
    is_array_child: bool,
    scope_id: u64,
    array_element_idx: i32,
    array_container_addr: u64,
) {
    let node = tree.nodes[node_idx as usize].clone();
    let abs_addr = resolve_addr(state, tree, node_idx, base, root_id);

    let type_w = state.effective_type_w(scope_id);
    let name_w = state.effective_name_w(scope_id);

    // Pointer deref expansion — merged fold header.
    if (node.kind == NodeKind::Pointer32 || node.kind == NodeKind::Pointer64) && node.ref_id != 0 {
        let ptr_target_name = resolve_pointer_target(tree, node.ref_id);
        let stars: String = "*".repeat((node.ptr_depth + 1) as usize);
        let mut ptr_type_override = format!(
            "{}{}",
            if ptr_target_name.is_empty() {
                "void"
            } else {
                &ptr_target_name
            },
            stars
        );
        if node.is_relative {
            ptr_type_override.push_str(" rva");
        }

        let ptr_children: Vec<i32> = state.child_indices(node.id).to_vec();
        let has_materialized = !ptr_children.is_empty();

        let force_collapsed = !has_materialized && state.virtual_ptr_refs.contains(&node.ref_id);
        let effective_collapsed = node.collapsed || force_collapsed;

        // Merged fold header.
        {
            let mut lm = LineMeta {
                node_idx,
                node_id: node.id,
                depth,
                line_kind: if effective_collapsed {
                    LineKind::Field
                } else {
                    LineKind::Header
                },
                offset_text: render::fmt_offset_margin(abs_addr, false, state.offset_hex_digits),
                offset_addr: abs_addr,
                ptr_base: state.current_ptr_base,
                node_kind: node.kind,
                fold_head: true,
                fold_collapsed: effective_collapsed,
                fold_level: compute_fold_level(depth, true),
                marker_mask: compute_markers(false),
                pointer_target_name: ptr_target_name.clone(),
                ..Default::default()
            };
            if force_collapsed {
                lm.marker_mask |= 1u32 << M_CYCLE;
            }
            let ptr_overflow = state.compact_columns && u16_len(&ptr_type_override) > type_w;
            lm.effective_type_w = if ptr_overflow {
                u16_len(&ptr_type_override)
            } else {
                type_w
            };
            lm.effective_name_w = name_w;

            let mut ptr_text = render::fmt_pointer_header(
                &node,
                depth,
                effective_collapsed,
                prov,
                abs_addr,
                &ptr_type_override,
                type_w,
                name_w,
                state.compact_columns,
            );

            // RTTI hint on typed-pointer headers: the pointer's value is *the
            // vtable address itself*. `compose_leaf`'s RTTI block doesn't see
            // this case (typed pointers route through `compose_node`), so we
            // duplicate the detect-and-attach here — same per-pass cache, same
            // `{RTTI: …}` text (`compose.cpp:1217-1241`). The PDB symbol rides
            // in the pointer-header value text via `read_value`
            // (`format.cpp:457`), so no separate Symbol chip.
            if prov.is_readable(abs_addr, 8) {
                let candidate = prov.read_u64(abs_addr);
                if candidate == 0 {
                    if state.show_rtti {
                        push_chip(
                            &mut ptr_text,
                            &mut lm,
                            ChipKind::Rtti,
                            "(Name class\u{2026})",
                            |c| {
                                c.rtti_vtable_addr = 0;
                            },
                        );
                    }
                } else if candidate != u64::MAX {
                    let info = rtti_for_vtable(state, prov, candidate);
                    if info.ok && !info.demangled_name.is_empty() {
                        let hint = format!("{{RTTI: {}}}", info.demangled_name);
                        push_chip(&mut ptr_text, &mut lm, ChipKind::Rtti, &hint, |c| {
                            c.rtti_vtable_addr = candidate;
                        });
                    }
                }
            }

            // NOTE: C++ `composeNode` (`compose.cpp:1213-1257`) attaches ONLY the
            // RTTI hint to a typed-pointer header — never a comment chip. A comment
            // on a pointer-to-class node is therefore invisible in the original, so
            // we deliberately do NOT push a Comment chip here (the leaf-field path
            // is the only place a node comment becomes a visible chip).

            if state.brace_wrap && !effective_collapsed && ptr_text.ends_with_unit(LBRACE) {
                ptr_text.chop();
                while ptr_text.ends_with_unit(SP) {
                    ptr_text.chop();
                }
                let saved_mask = lm.marker_mask;
                state.emit_line(&ptr_text, &mut lm);
                let mut brace_lm = LineMeta {
                    node_idx,
                    node_id: node.id,
                    depth,
                    line_kind: LineKind::Footer,
                    fold_level: compute_fold_level(depth, true),
                    marker_mask: saved_mask,
                    ..Default::default()
                };
                let mut t = U16Str::from_str(&render::indent(depth));
                t.push_str("{");
                state.emit_line(&t, &mut brace_lm);
            } else {
                state.emit_line(&ptr_text, &mut lm);
            }
        }

        if !effective_collapsed {
            let sz = node.byte_size();
            let mut ptr_val = 0u64;
            if prov.is_valid() && sz > 0 && prov.is_readable(abs_addr, sz) {
                ptr_val = if node.kind == NodeKind::Pointer32 {
                    prov.read_u32(abs_addr) as u64
                } else {
                    prov.read_u64(abs_addr)
                };
                if ptr_val != 0
                    && (ptr_val == u64::MAX
                        || (node.kind == NodeKind::Pointer32 && ptr_val == 0xFFFF_FFFF))
                {
                    ptr_val = 0;
                }
            }

            if node.is_relative && ptr_val != 0 {
                ptr_val = ptr_val.wrapping_add(base);
            }

            // Follow extra indirection levels (** struct pointers).
            let mut d = 0;
            while d < node.ptr_depth && ptr_val != 0 {
                let is64 = node.kind == NodeKind::Pointer64;
                let psz = if is64 { 8 } else { 4 };
                if !prov.is_readable(ptr_val, psz) {
                    ptr_val = 0;
                    break;
                }
                ptr_val = if is64 {
                    prov.read_u64(ptr_val)
                } else {
                    prov.read_u32(ptr_val) as u64
                };
                if ptr_val == u64::MAX || (!is64 && ptr_val == 0xFFFF_FFFF) {
                    ptr_val = 0;
                }
                d += 1;
            }

            let mut p_base = ptr_val;
            let ptr_readable = ptr_val != 0 && prov.is_readable(p_base, 1);
            let null_prov = NullProvider;
            let child_prov: &dyn Provider = if ptr_readable { prov } else { &null_prov };
            if !ptr_readable {
                p_base = 0;
            }

            let saved_ptr_base = state.current_ptr_base;
            state.current_ptr_base = p_base;

            if has_materialized {
                let n = ptr_children.len();
                for (pci, &cidx) in ptr_children.iter().enumerate() {
                    state.set_tree_sibling(depth + 1, pci < n - 1);
                    compose_node(
                        state,
                        tree,
                        child_prov,
                        cidx,
                        depth + 1,
                        p_base,
                        node.id,
                        false,
                        node.id,
                        -1,
                        0,
                    );
                }
            } else {
                let key = p_base ^ node.ref_id.wrapping_mul(GOLDEN_RATIO);
                if !state.ptr_visiting.contains(&key) {
                    state.ptr_visiting.insert(key);
                    let ref_idx = tree.index_of_id(node.ref_id);
                    if ref_idx >= 0 {
                        let ref_kind = tree.nodes[ref_idx as usize].kind;
                        let ref_id = tree.nodes[ref_idx as usize].id;
                        if ref_kind == NodeKind::Struct || ref_kind == NodeKind::Array {
                            let was_visiting = state.visiting.remove(&node.ref_id);
                            state.virtual_ptr_refs.insert(node.ref_id);
                            compose_parent(
                                state, tree, child_prov, ref_idx, depth, p_base, ref_id, true, 0,
                                -1, 0,
                            );
                            state.virtual_ptr_refs.remove(&node.ref_id);
                            if was_visiting {
                                state.visiting.insert(node.ref_id);
                            }
                        }
                    }
                    state.ptr_visiting.remove(&key);
                }
            }

            state.current_ptr_base = saved_ptr_base;

            // Footer for pointer fold. A typed pointer to a class shows the same
            // add-bytes pills as a struct footer (`+1 +10h +100h +1000h Trim Top`)
            // so the user can grow the pointed-to class definition from here — the
            // editor's footer-click resolves `node.ref_id` as the grow target. A
            // void/untyped pointer (no ref_id) keeps the plain closing brace.
            {
                let mut lm = LineMeta {
                    node_idx,
                    node_id: node.id,
                    depth,
                    line_kind: LineKind::Footer,
                    node_kind: node.kind,
                    fold_level: compute_fold_level(depth, false),
                    marker_mask: 0,
                    ..Default::default()
                };
                lm.offset_text.clear();
                let t = if node.ref_id != 0 {
                    render::fmt_struct_footer(&node, depth, tree.struct_span(node.ref_id))
                } else {
                    let mut t = U16Str::from_str(&render::indent(depth));
                    t.push_str("}");
                    t
                };
                state.emit_line(&t, &mut lm);
            }
        }
        return;
    }

    if node.kind == NodeKind::Struct || node.kind == NodeKind::Array {
        compose_parent(
            state,
            tree,
            prov,
            node_idx,
            depth,
            base,
            root_id,
            is_array_child,
            scope_id,
            array_element_idx,
            array_container_addr,
        );
    } else {
        compose_leaf(state, tree, prov, node_idx, depth, abs_addr, scope_id);
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Column geometry + clickable spans (`core.h:1119-1452`). The composition
// engine owns the column math; these mirror the C++ inline helpers verbatim.
// ───────────────────────────────────────────────────────────────────────────

/// `struct ColumnSpan` (`core.h:1119-1123`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ColumnSpan {
    pub start: i32,
    pub end: i32,
    pub valid: bool,
}

/// `enum class EditTarget` (`core.h:1125-1127`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EditTarget {
    Name,
    Type,
    Value,
    BaseAddress,
    Source,
    ArrayIndex,
    ArrayCount,
    ArrayElementType,
    ArrayElementCount,
    PointerTarget,
    RootClassType,
    RootClassName,
    TypeSelector,
    StaticExpr,
    Comment,
}

/// `struct LineGeometry` (`core.h:1155-1181`).
#[derive(Copy, Clone, Debug)]
pub struct LineGeometry {
    pub prefix_width: i32,
    pub indent_width: i32,
    pub type_column_width: i32,
    pub name_column_width: i32,
}

impl LineGeometry {
    pub fn type_start(&self) -> i32 {
        self.prefix_width + self.indent_width
    }
    pub fn name_start(&self) -> i32 {
        self.type_start() + self.type_column_width + K_SEP_WIDTH
    }
    pub fn value_start(&self) -> i32 {
        self.name_start() + self.name_column_width + K_SEP_WIDTH
    }
    pub fn document_column(&self, content_col: i32) -> i32 {
        self.prefix_width + content_col
    }

    /// `LineGeometry::forLine(lm)` (`core.h:1171-1180`).
    pub fn for_line(lm: &LineMeta) -> LineGeometry {
        let flush_left = lm.line_kind == LineKind::CommandRow
            || (lm.line_kind == LineKind::Footer && lm.is_root_header);
        LineGeometry {
            prefix_width: if flush_left { 0 } else { K_FOLD_COL },
            indent_width: lm.depth * K_TREE_INDENT,
            type_column_width: if lm.effective_type_w > 0 {
                lm.effective_type_w
            } else {
                K_COL_TYPE
            },
            name_column_width: if lm.effective_name_w > 0 {
                lm.effective_name_w
            } else {
                K_COL_NAME
            },
        }
    }
}

// Helpers to find a unit/substring index within a UTF-16 string view.
fn units(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}
fn index_of_unit(u: &[u16], target: u16, from: i32) -> i32 {
    let from = from.max(0) as usize;
    for (i, &c) in u.iter().enumerate().skip(from) {
        if c == target {
            return i as i32;
        }
    }
    -1
}
fn last_index_of_unit(u: &[u16], target: u16) -> i32 {
    for i in (0..u.len()).rev() {
        if u[i] == target {
            return i as i32;
        }
    }
    -1
}
fn index_of_str(u: &[u16], pat: &[u16], from: i32) -> i32 {
    if pat.is_empty() || pat.len() > u.len() {
        return -1;
    }
    let from = from.max(0) as usize;
    let end = u.len() - pat.len();
    for i in from..=end {
        if u[i..i + pat.len()] == *pat {
            return i as i32;
        }
    }
    -1
}
fn last_index_of_str(u: &[u16], pat: &[u16]) -> i32 {
    if pat.is_empty() || pat.len() > u.len() {
        return -1;
    }
    for i in (0..=(u.len() - pat.len())).rev() {
        if u[i..i + pat.len()] == *pat {
            return i as i32;
        }
    }
    -1
}
fn is_letter_or_number(u: u16) -> bool {
    char::from_u32(u as u32)
        .map(|c| c.is_alphanumeric())
        .unwrap_or(false)
}
fn is_space(u: u16) -> bool {
    char::from_u32(u as u32)
        .map(|c| c.is_whitespace())
        .unwrap_or(false)
}

/// `typeSpanFor` (`core.h:1183-1187`).
pub fn type_span_for(lm: &LineMeta, type_w: i32) -> ColumnSpan {
    if lm.line_kind != LineKind::Field || lm.is_continuation || lm.is_member_line {
        return ColumnSpan::default();
    }
    let ind = K_FOLD_COL + lm.depth * K_TREE_INDENT;
    ColumnSpan {
        start: ind,
        end: ind + type_w,
        valid: true,
    }
}

/// `nameSpanFor` (`core.h:1189-1200`).
pub fn name_span_for(lm: &LineMeta, type_w: i32, name_w: i32) -> ColumnSpan {
    if lm.is_continuation || lm.line_kind != LineKind::Field || lm.is_member_line {
        return ColumnSpan::default();
    }
    let ind = K_FOLD_COL + lm.depth * K_TREE_INDENT;
    let start = ind + type_w + K_SEP_WIDTH;
    ColumnSpan {
        start,
        end: start + name_w,
        valid: true,
    }
}

/// `valueSpanFor` (`core.h:1202-1222`).
pub fn value_span_for(lm: &LineMeta, type_w: i32, name_w: i32) -> ColumnSpan {
    if matches!(
        lm.line_kind,
        LineKind::Header | LineKind::Footer | LineKind::ArrayElementSeparator
    ) {
        return ColumnSpan::default();
    }
    if lm.is_member_line {
        return ColumnSpan::default();
    }
    let ind = K_FOLD_COL + lm.depth * K_TREE_INDENT;
    let is_hex = is_hex_preview(lm.node_kind);
    let val_width = if is_hex { 23 } else { K_COL_VALUE };
    let prefix_w = type_w + name_w + 2 * K_SEP_WIDTH;
    if lm.is_continuation {
        let start = ind + prefix_w;
        return ColumnSpan {
            start,
            end: start + val_width,
            valid: true,
        };
    }
    if lm.line_kind != LineKind::Field {
        return ColumnSpan::default();
    }
    let start = ind + prefix_w;
    ColumnSpan {
        start,
        end: start + val_width,
        valid: true,
    }
}

/// `memberNameSpanFor` (`core.h:1225-1233`).
pub fn member_name_span_for(lm: &LineMeta, line_text: &str) -> ColumnSpan {
    if !lm.is_member_line {
        return ColumnSpan::default();
    }
    let u = units(line_text);
    let ind = K_FOLD_COL + lm.depth * K_TREE_INDENT;
    let eq = index_of_str(&u, &units(" = "), ind);
    if eq < 0 {
        return ColumnSpan::default();
    }
    let mut name_end = eq;
    while name_end > ind && u[(name_end - 1) as usize] == SP {
        name_end -= 1;
    }
    ColumnSpan {
        start: ind,
        end: name_end,
        valid: true,
    }
}

/// `memberValueSpanFor` (`core.h:1235-1243`).
pub fn member_value_span_for(lm: &LineMeta, line_text: &str) -> ColumnSpan {
    if !lm.is_member_line {
        return ColumnSpan::default();
    }
    let u = units(line_text);
    let eq = index_of_str(&u, &units(" = "), 0);
    if eq < 0 {
        return ColumnSpan::default();
    }
    let val_start = eq + 3;
    let mut val_end = u.len() as i32;
    while val_end > val_start && u[(val_end - 1) as usize] == SP {
        val_end -= 1;
    }
    ColumnSpan {
        start: val_start,
        end: val_end,
        valid: true,
    }
}

/// `staticExprSpanFor` (`core.h:1246-1261`).
pub fn static_expr_span_for(line_text: &str) -> ColumnSpan {
    let u = units(line_text);
    let ret = index_of_str(&u, &units("return "), 0);
    if ret < 0 {
        return ColumnSpan::default();
    }
    let expr_start = ret + 7;
    let mut expr_end = u.len() as i32;
    let arrow = index_of_unit(&u, 0x2192, expr_start);
    if arrow > expr_start {
        expr_end = arrow;
    }
    let err = index_of_str(&u, &units("(error)"), expr_start);
    if err > expr_start && err < expr_end {
        expr_end = err;
    }
    let brace = index_of_str(&u, &units(" }"), expr_start);
    if brace > expr_start && brace < expr_end {
        expr_end = brace;
    }
    while expr_end > expr_start && u[(expr_end - 1) as usize] == SP {
        expr_end -= 1;
    }
    ColumnSpan {
        start: expr_start,
        end: expr_end,
        valid: true,
    }
}

/// `commentSpanFor` (`core.h:1263-1280`).
pub fn comment_span_for(lm: &LineMeta, line_length: i32, type_w: i32, name_w: i32) -> ColumnSpan {
    if matches!(
        lm.line_kind,
        LineKind::Header
            | LineKind::Footer
            | LineKind::CommandRow
            | LineKind::ArrayElementSeparator
    ) || lm.is_continuation
        || lm.is_member_line
    {
        return ColumnSpan::default();
    }
    let ind = K_FOLD_COL + lm.depth * K_TREE_INDENT;
    let is_hex = is_hex_preview(lm.node_kind);
    let val_width = if is_hex { 23 } else { K_COL_VALUE };
    let prefix_w = type_w + name_w + 2 * K_SEP_WIDTH;
    let start = ind + prefix_w + val_width;
    ColumnSpan {
        start,
        end: line_length,
        valid: start < line_length,
    }
}

/// `commandRowSrcSpan` (`core.h:1285-1294`).
pub fn command_row_src_span(line_text: &str) -> ColumnSpan {
    let u = units(line_text);
    let arrow = index_of_unit(&u, 0x25BE, 0);
    if arrow < 0 {
        return ColumnSpan::default();
    }
    let mut start = 0i32;
    while start < arrow {
        let c = u[start as usize];
        if !is_letter_or_number(c) && c != b'<' as u16 && c != b'\'' as u16 {
            start += 1;
        } else {
            break;
        }
    }
    if start >= arrow {
        return ColumnSpan::default();
    }
    ColumnSpan {
        start,
        end: arrow,
        valid: true,
    }
}

/// `commandRowRootStart` (`core.h:1299-1310`).
fn command_row_root_start(u: &[u16]) -> i32 {
    let mut best = -1i32;
    for pat in ["struct ", "class ", "enum "] {
        let i = last_index_of_str(u, &units(pat));
        if i > best {
            best = i;
        }
    }
    best
}

/// `commandRowAddrSpan` (`core.h:1312-1336`).
pub fn command_row_addr_span(line_text: &str) -> ColumnSpan {
    let u = units(line_text);
    let arrow = index_of_unit(&u, 0x25BE, 0);
    if arrow < 0 {
        return ColumnSpan::default();
    }
    let mut addr_start = arrow + 1;
    while (addr_start as usize) < u.len() && is_space(u[addr_start as usize]) {
        addr_start += 1;
    }
    let start;
    if (addr_start as usize) < u.len()
        && (u[addr_start as usize] == b'<' as u16 || u[addr_start as usize] == b'[' as u16)
    {
        start = addr_start;
    } else {
        let ox_pos = index_of_str(&u, &units("0x"), arrow);
        start = if ox_pos >= 0 { ox_pos } else { addr_start };
    }
    let root_start = command_row_root_start(&u);
    let mut end = if root_start > start {
        root_start
    } else {
        u.len() as i32
    };
    while end > start && is_space(u[(end - 1) as usize]) {
        end -= 1;
    }
    if end <= start {
        return ColumnSpan::default();
    }
    ColumnSpan {
        start,
        end,
        valid: true,
    }
}

/// `commandRowRootTypeSpan` (`core.h:1338-1345`).
pub fn command_row_root_type_span(line_text: &str) -> ColumnSpan {
    let u = units(line_text);
    let start = command_row_root_start(&u);
    if start < 0 {
        return ColumnSpan::default();
    }
    let mut end = start;
    while (end as usize) < u.len() && u[end as usize] != SP {
        end += 1;
    }
    if end <= start {
        return ColumnSpan::default();
    }
    ColumnSpan {
        start,
        end,
        valid: true,
    }
}

/// `commandRowRootNameSpan` (`core.h:1347-1360`).
pub fn command_row_root_name_span(line_text: &str) -> ColumnSpan {
    let u = units(line_text);
    let base = command_row_root_start(&u);
    if base < 0 {
        return ColumnSpan::default();
    }
    let space = index_of_unit(&u, SP, base);
    if space < 0 {
        return ColumnSpan::default();
    }
    let mut name_start = space + 1;
    while (name_start as usize) < u.len() && is_space(u[name_start as usize]) {
        name_start += 1;
    }
    if name_start as usize >= u.len() {
        return ColumnSpan::default();
    }
    let mut name_end = index_of_str(&u, &units(" {"), name_start);
    if name_end < 0 {
        name_end = u.len() as i32;
    }
    while name_end > name_start && is_space(u[(name_end - 1) as usize]) {
        name_end -= 1;
    }
    if name_end <= name_start {
        return ColumnSpan::default();
    }
    ColumnSpan {
        start: name_start,
        end: name_end,
        valid: true,
    }
}

/// `commandRowChevronSpan` (`core.h:1365-1370`).
pub fn command_row_chevron_span(line_text: &str) -> ColumnSpan {
    let u = units(line_text);
    if u.len() < 3 {
        return ColumnSpan::default();
    }
    if u[0] == b'[' as u16 && u[1] == 0x25B8 && u[2] == b']' as u16 {
        return ColumnSpan {
            start: 0,
            end: 4.min(u.len() as i32),
            valid: true,
        };
    }
    ColumnSpan::default()
}

/// `arrayElemTypeSpanFor` (`core.h:1376-1383`).
pub fn array_elem_type_span_for(lm: &LineMeta, line_text: &str) -> ColumnSpan {
    if lm.line_kind != LineKind::Header || !lm.is_array_header {
        return ColumnSpan::default();
    }
    let u = units(line_text);
    let ind = K_FOLD_COL + lm.depth * K_TREE_INDENT;
    let bracket = index_of_unit(&u, b'[' as u16, ind);
    if bracket <= ind {
        return ColumnSpan::default();
    }
    ColumnSpan {
        start: ind,
        end: bracket,
        valid: true,
    }
}

/// `arrayElemCountSpanFor` (`core.h:1385-1392`).
pub fn array_elem_count_span_for(lm: &LineMeta, line_text: &str) -> ColumnSpan {
    if lm.line_kind != LineKind::Header || !lm.is_array_header {
        return ColumnSpan::default();
    }
    let u = units(line_text);
    let ind = K_FOLD_COL + lm.depth * K_TREE_INDENT;
    let open = index_of_unit(&u, b'[' as u16, ind);
    let close = index_of_unit(&u, b']' as u16, open);
    if open < 0 || close < 0 || close <= open + 1 {
        return ColumnSpan::default();
    }
    ColumnSpan {
        start: open + 1,
        end: close,
        valid: true,
    }
}

/// `arrayElemCountClickSpanFor` (`core.h:1395-1402`).
pub fn array_elem_count_click_span_for(lm: &LineMeta, line_text: &str) -> ColumnSpan {
    if lm.line_kind != LineKind::Header || !lm.is_array_header {
        return ColumnSpan::default();
    }
    let u = units(line_text);
    let ind = K_FOLD_COL + lm.depth * K_TREE_INDENT;
    let open = index_of_unit(&u, b'[' as u16, ind);
    let close = index_of_unit(&u, b']' as u16, open);
    if open < 0 || close < 0 || close <= open + 1 {
        return ColumnSpan::default();
    }
    ColumnSpan {
        start: open,
        end: close + 1,
        valid: true,
    }
}

/// `pointerKindSpanFor` (`core.h:1408-1410`) — always invalid (Type* format).
pub fn pointer_kind_span_for(_lm: &LineMeta, _line_text: &str) -> ColumnSpan {
    ColumnSpan::default()
}

/// `pointerTargetSpanFor` (`core.h:1412-1419`).
pub fn pointer_target_span_for(lm: &LineMeta, line_text: &str) -> ColumnSpan {
    if (lm.line_kind != LineKind::Field && lm.line_kind != LineKind::Header) || lm.is_continuation {
        return ColumnSpan::default();
    }
    if lm.node_kind != NodeKind::Pointer32 && lm.node_kind != NodeKind::Pointer64 {
        return ColumnSpan::default();
    }
    let u = units(line_text);
    let ind = K_FOLD_COL + lm.depth * K_TREE_INDENT;
    let star = index_of_unit(&u, b'*' as u16, ind);
    if star <= ind {
        return ColumnSpan::default();
    }
    ColumnSpan {
        start: ind,
        end: star,
        valid: true,
    }
}

/// `arrayPrevSpanFor` (`core.h:1424-1429`).
pub fn array_prev_span_for(lm: &LineMeta, line_text: &str) -> ColumnSpan {
    if !lm.is_array_header {
        return ColumnSpan::default();
    }
    let u = units(line_text);
    let lt = last_index_of_unit(&u, b'<' as u16);
    if lt < 0 {
        return ColumnSpan::default();
    }
    ColumnSpan {
        start: lt,
        end: lt + 1,
        valid: true,
    }
}

/// `arrayIndexSpanFor` (`core.h:1431-1437`).
pub fn array_index_span_for(lm: &LineMeta, line_text: &str) -> ColumnSpan {
    if !lm.is_array_header {
        return ColumnSpan::default();
    }
    let u = units(line_text);
    let lt = last_index_of_unit(&u, b'<' as u16);
    let slash = index_of_unit(&u, b'/' as u16, lt);
    if lt < 0 || slash < 0 {
        return ColumnSpan::default();
    }
    ColumnSpan {
        start: lt + 1,
        end: slash,
        valid: true,
    }
}

/// `arrayCountSpanFor` (`core.h:1439-1445`).
pub fn array_count_span_for(lm: &LineMeta, line_text: &str) -> ColumnSpan {
    if !lm.is_array_header {
        return ColumnSpan::default();
    }
    let u = units(line_text);
    let slash = last_index_of_unit(&u, b'/' as u16);
    let gt = index_of_unit(&u, b'>' as u16, slash);
    if slash < 0 || gt < 0 {
        return ColumnSpan::default();
    }
    ColumnSpan {
        start: slash + 1,
        end: gt,
        valid: true,
    }
}

/// `arrayNextSpanFor` (`core.h:1447-1451`).
pub fn array_next_span_for(lm: &LineMeta, line_text: &str) -> ColumnSpan {
    if !lm.is_array_header {
        return ColumnSpan::default();
    }
    let u = units(line_text);
    let gt = last_index_of_unit(&u, b'>' as u16);
    if gt < 0 {
        return ColumnSpan::default();
    }
    ColumnSpan {
        start: gt,
        end: gt + 1,
        valid: true,
    }
}

// ───────────────────────────────────────────────────────────────────────────
// render — the `fmt::` value/line rendering routines (`format.cpp`). These are
// the rendering helpers `compose.cpp` consumes; they are compose-internal here.
// The public `crate::format` module owns the editable/parse API surface; this
// module owns the display-side column geometry it shares with the renderer.
// ───────────────────────────────────────────────────────────────────────────

/// Live-editor rendering facade.
///
/// In the C++ original there is exactly ONE render layer (`src/format.cpp`,
/// the `rcx::fmt` namespace); `src/compose.cpp` performs no independent value
/// rendering — it calls `fmt::*` ~64 times. This module mirrors that: every
/// function here is a thin delegation to [`crate::format`], so the live-editor
/// path and the pure presentation layer can never diverge again (the previous
/// duplicate copy rounded half-to-even and dropped the `%g` exponent carry).
///
/// The four `U16Str`-returning fns wrap `format`'s `String` output; the rest
/// re-export or forward directly. Signatures are kept byte-for-byte identical
/// to the previous copy so the ~40 `render::` call-sites in this file are
/// untouched.
mod render {
    use super::U16Str;
    use crate::core::{Node, NodeKind};
    use crate::provider::Provider;

    pub use crate::format::{
        array_type_name, extract_bits, fmt_offset_margin, indent, struct_type_name, type_name_raw,
    };

    /// `pointerTypeName(kind, targetName)` (`format.cpp:117-121`) — for width.
    /// The C++ `kind` arg is unused; pass `Pointer64` to match the facade.
    pub fn pointer_type_name(target_name: String) -> String {
        crate::format::pointer_type_name(NodeKind::Pointer64, &target_name)
    }

    /// Alias matching the static-field call site (`compose.cpp:1041`).
    pub fn pointer_type_name_kind(target_name: String) -> String {
        pointer_type_name(target_name)
    }

    /// `fmtStructHeader(...)` (`format.cpp:248-259`).
    pub fn fmt_struct_header(
        node: &Node,
        depth: i32,
        collapsed: bool,
        col_type: i32,
        col_name: i32,
        compact: bool,
    ) -> U16Str {
        U16Str::from_str(&crate::format::fmt_struct_header(
            node, depth, collapsed, col_type, col_name, compact,
        ))
    }

    /// `fmtStructFooter(node, depth, totalSize)` (`format.cpp:261-272`).
    pub fn fmt_struct_footer(node: &Node, depth: i32, total_size: i32) -> U16Str {
        U16Str::from_str(&crate::format::fmt_struct_footer(node, depth, total_size))
    }

    /// `fmtArrayHeader(...)` (`format.cpp:276-282`). The C++ `viewIdx` arg does
    /// not affect output; pass `0`.
    #[allow(clippy::too_many_arguments)]
    pub fn fmt_array_header(
        node: &Node,
        depth: i32,
        collapsed: bool,
        col_type: i32,
        col_name: i32,
        elem_struct_name: &str,
        compact: bool,
    ) -> U16Str {
        U16Str::from_str(&crate::format::fmt_array_header(
            node,
            depth,
            0,
            collapsed,
            col_type,
            col_name,
            elem_struct_name,
            compact,
        ))
    }

    /// `fmtPointerHeader(...)` (`format.cpp:286-304`).
    #[allow(clippy::too_many_arguments)]
    pub fn fmt_pointer_header(
        node: &Node,
        depth: i32,
        collapsed: bool,
        prov: &dyn Provider,
        addr: u64,
        ptr_type_name: &str,
        col_type: i32,
        col_name: i32,
        compact: bool,
    ) -> U16Str {
        U16Str::from_str(&crate::format::fmt_pointer_header(
            node,
            depth,
            collapsed,
            prov,
            addr,
            ptr_type_name,
            col_type,
            col_name,
            compact,
        ))
    }

    /// `fmtNodeLine(...)` (`format.cpp:512-556`).
    #[allow(clippy::too_many_arguments)]
    pub fn fmt_node_line(
        node: &Node,
        prov: &dyn Provider,
        addr: u64,
        depth: i32,
        sub_line: i32,
        comment: &str,
        col_type: i32,
        col_name: i32,
        type_override: &str,
        compact: bool,
    ) -> U16Str {
        U16Str::from_str(&crate::format::fmt_node_line(
            node,
            prov,
            addr,
            depth,
            sub_line,
            comment,
            col_type,
            col_name,
            type_override,
            compact,
        ))
    }

    /// `fmtEnumMember(name, value, depth, nameW)` (`format.cpp:907-910`).
    pub fn fmt_enum_member(name: &str, value: i64, depth: i32, name_w: i32) -> U16Str {
        U16Str::from_str(&crate::format::fmt_enum_member(name, value, depth, name_w))
    }

    /// `fmtBitfieldMember(...)` (`format.cpp:929-934`).
    pub fn fmt_bitfield_member(
        name: &str,
        bit_width: u8,
        value: u64,
        depth: i32,
        name_w: i32,
    ) -> U16Str {
        U16Str::from_str(&crate::format::fmt_bitfield_member(
            name, bit_width, value, depth, name_w,
        ))
    }
}

#[cfg(test)]
mod tests;
