//! Tab edit-target cycling within a row (editor-surface.md §10 `handleNormalKey`
//! Tab branch + §17.8). Pure logic, unit-tested.
//!
//! Tab cycles the inline-editable targets in a **fixed order**, starting after
//! the last target the user edited (`m_lastTabTarget`), skipping any target that
//! does not apply to the current line (array/pointer/hex gating). The first
//! target whose span resolves on the line is the one to edit; `m_lastTabTarget`
//! then advances to it so a subsequent Tab continues the cycle.

use crate::compose::EditTarget;
use crate::core::{is_hex_preview, LineKind, LineMeta, NodeKind};

/// The fixed Tab cycle order (`handleNormalKey` line 5804):
/// `{Name, Value, Comment, ArrayElementType, ArrayElementCount, PointerTarget, Type}`.
pub const TAB_ORDER: [EditTarget; 7] = [
    EditTarget::Name,
    EditTarget::Value,
    EditTarget::Comment,
    EditTarget::ArrayElementType,
    EditTarget::ArrayElementCount,
    EditTarget::PointerTarget,
    EditTarget::Type,
];

/// Whether `target` is an applicable edit target on the given line — the gating
/// the Tab cycle uses to skip inapplicable targets (editor-surface.md §10). This
/// is the *structural* applicability (does this kind of line have this field at
/// all); the precise column span is resolved separately by
/// [`super::geometry::resolved_span_for`].
pub fn target_applies(lm: &LineMeta, target: EditTarget) -> bool {
    let is_hex = is_hex_preview(lm.node_kind);
    let is_ptr = matches!(lm.node_kind, NodeKind::Pointer32 | NodeKind::Pointer64);
    match target {
        // Hex rows block Name/Value editing (§17.2); footer/command/sep have none.
        EditTarget::Name => {
            matches!(lm.line_kind, LineKind::Field | LineKind::Header)
                && !is_hex
                && !lm.is_continuation
        }
        EditTarget::Value => lm.line_kind == LineKind::Field && !is_hex && !lm.is_continuation,
        EditTarget::Comment => {
            matches!(lm.line_kind, LineKind::Field) && !lm.is_member_line && !lm.is_continuation
        }
        EditTarget::ArrayElementType | EditTarget::ArrayElementCount => {
            lm.line_kind == LineKind::Header && lm.is_array_header
        }
        EditTarget::PointerTarget => {
            matches!(lm.line_kind, LineKind::Field | LineKind::Header) && is_ptr
        }
        EditTarget::Type => {
            // Type is editable on field/header rows (incl. hex — only Type is).
            matches!(lm.line_kind, LineKind::Field | LineKind::Header)
                && !lm.is_continuation
                && !lm.is_member_line
        }
        _ => false,
    }
}

/// Given the last-edited target (or `None` to start from the top), return the
/// next applicable target in [`TAB_ORDER`], wrapping once. Returns `None` if the
/// line has no editable target at all. Port of the Tab-cycle loop
/// (editor-surface.md §10 line 5804; §17.8 "Tab cycles edit targets in fixed
/// order, skipping inapplicable").
pub fn next_tab_target(lm: &LineMeta, last: Option<EditTarget>) -> Option<EditTarget> {
    let start = match last {
        Some(t) => TAB_ORDER
            .iter()
            .position(|&x| x == t)
            .map(|i| i + 1)
            .unwrap_or(0),
        None => 0,
    };
    for offset in 0..TAB_ORDER.len() {
        let idx = (start + offset) % TAB_ORDER.len();
        let target = TAB_ORDER[idx];
        if target_applies(lm, target) {
            return Some(target);
        }
    }
    None
}

/// Reverse Tab (Shift+Tab, item 17): the next applicable target walking [`TAB_ORDER`]
/// **backward** from before `last`, wrapping once. `None` to start from the bottom.
/// Returns `None` if the line has no editable target. The forward cycle ignored its
/// `backward` flag; this is the true reverse traversal it should use.
pub fn prev_tab_target(lm: &LineMeta, last: Option<EditTarget>) -> Option<EditTarget> {
    let n = TAB_ORDER.len();
    // Start one BEFORE `last` (wrapping); with no `last`, start at the bottom.
    let start = match last {
        Some(t) => TAB_ORDER
            .iter()
            .position(|&x| x == t)
            .map(|i| (i + n - 1) % n)
            .unwrap_or(n - 1),
        None => n - 1,
    };
    for offset in 0..n {
        let idx = (start + n - offset) % n;
        let target = TAB_ORDER[idx];
        if target_applies(lm, target) {
            return Some(target);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(kind: NodeKind) -> LineMeta {
        LineMeta {
            line_kind: LineKind::Field,
            node_kind: kind,
            ..LineMeta::default()
        }
    }

    #[test]
    fn plain_field_cycles_name_value_comment_type() {
        let lm = field(NodeKind::Int32);
        // From the top → Name.
        assert_eq!(next_tab_target(&lm, None), Some(EditTarget::Name));
        // After Name → Value.
        assert_eq!(
            next_tab_target(&lm, Some(EditTarget::Name)),
            Some(EditTarget::Value)
        );
        // After Value → Comment.
        assert_eq!(
            next_tab_target(&lm, Some(EditTarget::Value)),
            Some(EditTarget::Comment)
        );
        // After Comment → (skip array/ptr) Type.
        assert_eq!(
            next_tab_target(&lm, Some(EditTarget::Comment)),
            Some(EditTarget::Type)
        );
        // After Type → wrap to Name.
        assert_eq!(
            next_tab_target(&lm, Some(EditTarget::Type)),
            Some(EditTarget::Name)
        );
    }

    #[test]
    fn hex_field_only_offers_type() {
        let lm = field(NodeKind::Hex64);
        // Name/Value blocked; Comment applies on a field row; Type applies.
        assert!(!target_applies(&lm, EditTarget::Name));
        assert!(!target_applies(&lm, EditTarget::Value));
        assert!(target_applies(&lm, EditTarget::Comment));
        assert!(target_applies(&lm, EditTarget::Type));
        // From the top, the first applicable is Comment (Name/Value skipped).
        assert_eq!(next_tab_target(&lm, None), Some(EditTarget::Comment));
        // After Comment → Type (array/ptr skipped).
        assert_eq!(
            next_tab_target(&lm, Some(EditTarget::Comment)),
            Some(EditTarget::Type)
        );
    }

    #[test]
    fn pointer_field_includes_pointer_target() {
        let lm = field(NodeKind::Pointer64);
        assert!(target_applies(&lm, EditTarget::PointerTarget));
        // After Comment → PointerTarget (array skipped, ptr present).
        assert_eq!(
            next_tab_target(&lm, Some(EditTarget::Comment)),
            Some(EditTarget::PointerTarget)
        );
    }

    #[test]
    fn array_header_includes_element_targets() {
        let lm = LineMeta {
            line_kind: LineKind::Header,
            is_array_header: true,
            node_kind: NodeKind::Array,
            ..LineMeta::default()
        };
        assert!(target_applies(&lm, EditTarget::ArrayElementType));
        assert!(target_applies(&lm, EditTarget::ArrayElementCount));
        // Header rows have a Name and Type, no Value.
        assert!(target_applies(&lm, EditTarget::Name));
        assert!(!target_applies(&lm, EditTarget::Value));
        // From the top: Name first.
        assert_eq!(next_tab_target(&lm, None), Some(EditTarget::Name));
        // After Comment (n/a on header but used as a cursor) → ArrayElementType.
        assert_eq!(
            next_tab_target(&lm, Some(EditTarget::Comment)),
            Some(EditTarget::ArrayElementType)
        );
    }

    #[test]
    fn footer_and_continuation_have_no_targets() {
        let footer = LineMeta {
            line_kind: LineKind::Footer,
            ..LineMeta::default()
        };
        assert_eq!(next_tab_target(&footer, None), None);

        let cont = LineMeta {
            line_kind: LineKind::Field,
            is_continuation: true,
            node_kind: NodeKind::Int32,
            ..LineMeta::default()
        };
        assert_eq!(next_tab_target(&cont, None), None);
    }

    #[test]
    fn unknown_last_target_restarts_from_top() {
        let lm = field(NodeKind::Int32);
        // BaseAddress isn't in TAB_ORDER → treated as "start from top".
        assert_eq!(
            next_tab_target(&lm, Some(EditTarget::BaseAddress)),
            Some(EditTarget::Name)
        );
    }

    #[test]
    fn reverse_tab_cycles_backward() {
        // Item 17: Shift+Tab walks the cycle backward (the mirror of the forward
        // test). On a plain field the applicable targets are Name/Value/Comment/Type.
        let lm = field(NodeKind::Int32);
        // From Value, the previous applicable is Name.
        assert_eq!(
            prev_tab_target(&lm, Some(EditTarget::Value)),
            Some(EditTarget::Name)
        );
        // From Name, wrap backward to Type (the last applicable).
        assert_eq!(
            prev_tab_target(&lm, Some(EditTarget::Name)),
            Some(EditTarget::Type)
        );
        // From Type, the previous applicable is Comment (array/ptr skipped).
        assert_eq!(
            prev_tab_target(&lm, Some(EditTarget::Type)),
            Some(EditTarget::Comment)
        );
        // No `last` → start from the bottom → Type.
        assert_eq!(prev_tab_target(&lm, None), Some(EditTarget::Type));
    }

    #[test]
    fn reverse_tab_skips_inapplicable_on_hex() {
        // Hex rows block Name/Value; backward from Type → Comment.
        let lm = field(NodeKind::Hex64);
        assert_eq!(
            prev_tab_target(&lm, Some(EditTarget::Type)),
            Some(EditTarget::Comment)
        );
        // Backward from Comment wraps to Type (Name/Value skipped).
        assert_eq!(
            prev_tab_target(&lm, Some(EditTarget::Comment)),
            Some(EditTarget::Type)
        );
    }
}
