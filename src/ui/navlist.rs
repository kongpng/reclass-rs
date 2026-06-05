//! gpui-free, section-aware list navigation over a `selectable` bool mask.
//!
//! The SourceChooser and TypeSelector models carried byte-identical Down / Up /
//! PageDown / PageUp / Home / End logic that skips non-selectable rows (section
//! headers, disabled entries). These free functions hold that one copy; each
//! model builds a `Vec<bool>` mask of its rows and delegates.

/// The next selectable index after `from` in direction `dir` (+1 down / -1 up),
/// skipping non-selectable rows. `None` if none remain that way.
pub fn next_selectable(mask: &[bool], from: usize, dir: i32) -> Option<usize> {
    let len = mask.len();
    if len == 0 {
        return None;
    }
    let mut i = from as i32 + dir;
    while i >= 0 && (i as usize) < len {
        if mask[i as usize] {
            return Some(i as usize);
        }
        i += dir;
    }
    None
}

/// The first selectable index, if any.
pub fn first_selectable(mask: &[bool]) -> Option<usize> {
    mask.iter().position(|&s| s)
}

/// The last selectable index, if any.
pub fn last_selectable(mask: &[bool]) -> Option<usize> {
    mask.iter().rposition(|&s| s)
}

/// One Down/Up step from `sel`, returning the new selection. Down (`dir > 0`)
/// falls back to the first selectable row when nothing is selected yet; Up
/// (`dir < 0`) is a no-op when nothing is selected. When no row remains in that
/// direction, the current selection is kept.
pub fn step(mask: &[bool], sel: Option<usize>, dir: i32) -> Option<usize> {
    if dir > 0 {
        let from = sel.unwrap_or(0);
        match next_selectable(mask, from, dir) {
            Some(n) => Some(n),
            None if sel.is_none() => first_selectable(mask),
            None => sel,
        }
    } else {
        let from = sel?;
        Some(next_selectable(mask, from, dir).unwrap_or(from))
    }
}

/// `page` Down/Up steps (PageDown/PageUp), stopping at the end of the run. Mirrors
/// repeatedly applying [`step`] but matching the originals' early-break shape.
pub fn page(mask: &[bool], sel: Option<usize>, dir: i32, page: usize) -> Option<usize> {
    let mut cur = sel;
    if dir > 0 {
        for _ in 0..page.max(1) {
            let from = cur.unwrap_or(0);
            match next_selectable(mask, from, dir) {
                Some(n) => cur = Some(n),
                None => {
                    if cur.is_none() {
                        cur = first_selectable(mask);
                    }
                    break;
                }
            }
        }
    } else {
        for _ in 0..page.max(1) {
            let Some(from) = cur else { break };
            match next_selectable(mask, from, dir) {
                Some(p) => cur = Some(p),
                None => break,
            }
        }
    }
    cur
}

#[cfg(test)]
mod tests {
    use super::*;

    // Row 0 = section header (not selectable), 1..=3 selectable, 4 header, 5 sel.
    const M: &[bool] = &[false, true, true, true, false, true];

    #[test]
    fn next_skips_non_selectable() {
        assert_eq!(next_selectable(M, 0, 1), Some(1));
        assert_eq!(next_selectable(M, 3, 1), Some(5)); // skips the header at 4
        assert_eq!(next_selectable(M, 5, 1), None);
        assert_eq!(next_selectable(M, 5, -1), Some(3));
        assert_eq!(next_selectable(M, 1, -1), None); // header at 0 not selectable
        assert_eq!(next_selectable(&[], 0, 1), None);
    }

    #[test]
    fn first_and_last() {
        assert_eq!(first_selectable(M), Some(1));
        assert_eq!(last_selectable(M), Some(5));
        assert_eq!(first_selectable(&[false, false]), None);
        assert_eq!(last_selectable(&[false, false]), None);
    }

    #[test]
    fn step_down_falls_back_to_first_when_unselected() {
        assert_eq!(step(M, None, 1), Some(1));
        assert_eq!(step(M, Some(1), 1), Some(2));
        assert_eq!(step(M, Some(5), 1), Some(5)); // at end, kept
    }

    #[test]
    fn step_up_is_noop_when_unselected() {
        assert_eq!(step(M, None, -1), None);
        assert_eq!(step(M, Some(5), -1), Some(3));
        assert_eq!(step(M, Some(1), -1), Some(1)); // at top, kept
    }

    #[test]
    fn page_walks_multiple_and_clamps() {
        assert_eq!(page(M, Some(1), 1, 10), Some(5)); // runs to last
        assert_eq!(page(M, Some(5), -1, 10), Some(1)); // runs to first
        assert_eq!(page(M, None, 1, 2), Some(2)); // none -> 1 -> 2
        assert_eq!(page(M, None, -1, 3), None); // up from none stays none
    }
}
