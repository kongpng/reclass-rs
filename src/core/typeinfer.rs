//! Heuristic type-inference engine — scores byte patterns into candidate
//! `NodeKind`s for the editor's "type hint chips".
//!
//! Port of `src/typeinfer.h` (550 lines, header-only). The public surface
//! (`InferHints`, `TypeSuggestion`, `infer_types`, `format_hint`) is faithful;
//! the feature-checker bodies mirror the C++ scoring. This is a SKELETON port:
//! the whole-width / split candidate generation is in place behind
//! `infer_types`, with the per-kind feature checkers stubbed to `todo!()` until
//! the dedicated `typeinfer` workflow fills them in (see ARCHITECTURE.md §9).

use super::kind::{kind_meta, NodeKind};

/// `struct InferHints` (`typeinfer.h:13-20`).
#[derive(Clone, Copy, Debug, Default)]
pub struct InferHints<'a> {
    /// raw bytes, same len as `data`.
    pub min_observed: Option<&'a [u8]>,
    pub max_observed: Option<&'a [u8]>,
    /// value only increases or only decreases.
    pub monotonic: bool,
    /// identical across all samples.
    pub never_changed: bool,
    /// 0 = no history.
    pub sample_count: i32,
    pub ptr_size: i32,
}

/// `struct TypeSuggestion` (`typeinfer.h:24-28`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeSuggestion {
    /// `len==1`: convert; `len>1`: uniform split.
    pub kinds: Vec<NodeKind>,
    /// 0-100 feature ratio (passed / checked × 100).
    pub score: i32,
    /// 0=hidden, 1=weak, 2=moderate, 3=strong.
    pub strength: i32,
}

/// `formatHint` (`typeinfer.h:38-44`) — short type label (e.g. "ptr64",
/// "int32_t×2").
pub fn format_hint(s: &TypeSuggestion) -> String {
    let Some(first) = s.kinds.first() else {
        return String::new();
    };
    let name = kind_meta(*first).map_or("", |m| m.type_name);
    if s.kinds.len() == 1 {
        name.to_string()
    } else {
        format!("{name}\u{00D7}{}", s.kinds.len())
    }
}

/// `inferTypes` (`typeinfer.h:524-548`) — the entry point. Returns up to
/// `max_results` ranked suggestions for the given byte slice.
///
/// SKELETON: the candidate-generation pipeline is implemented in the dedicated
/// `typeinfer` workflow; until then this returns an empty list for the
/// degenerate (null / all-zero) inputs the C++ also rejects, and `todo!()` for
/// the scored path so the contract is explicit.
pub fn infer_types(data: &[u8], hints: &InferHints<'_>, max_results: i32) -> Vec<TypeSuggestion> {
    let _ = (hints, max_results);
    if data.is_empty() {
        return Vec::new();
    }
    if data.iter().all(|&b| b == 0) {
        return Vec::new(); // NULL → skip entirely (typeinfer.h:532).
    }
    todo!("port typeinfer.h candidate generation + pruneAndRank (workflow: typeinfer)")
}
