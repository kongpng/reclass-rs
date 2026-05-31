//! Tree + Provider → rendered rows (text + `LineMeta`, column geometry).
//!
//! Port of `src/compose.cpp`. **SKELETON** — the column geometry and the full
//! row-by-row composition are filled in by the dedicated `compose` workflow
//! (ARCHITECTURE.md §9). The public entry point signature is in place.

use crate::core::{ComposeResult, NodeTree};
use crate::provider::Provider;

/// `rcx::compose(...)` (`core.h:1497-1514`, defined in `compose.cpp`).
///
/// Renders `tree` against the live `provider` into a [`ComposeResult`] (text +
/// per-line metadata + layout). The trailing booleans mirror the C++ defaults.
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
    let _ = (
        tree,
        provider,
        view_root_id,
        compact_columns,
        tree_lines,
        brace_wrap,
        type_hints,
        show_comments,
        show_rtti,
        show_enum_chips,
    );
    todo!("port compose.cpp (workflow: compose-undo)")
}
