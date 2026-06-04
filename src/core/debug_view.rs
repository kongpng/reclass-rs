//! Debug-view text generator — the read-only developer dump of the composed
//! line/`LineMeta` model.
//!
//! Faithful, pure-text port of `MainWindow::generateDebugText`
//! (`src/main.cpp:5534-5595`). Given a [`ComposeResult`] (the composer's
//! `text` + parallel `meta`), it reproduces the C++ debug listing **byte for
//! byte**: one output line per composed line, each formatted as
//!
//! ```text
//! <offsetText>|<annotated line text>  ## L=<i> <LineKind> nKind=<kind> \
//!     depth=<d> cmtStart=<c> tW=<tw> nW=<nw>[ static][ cont][ member]\
//!     [ arrElem][ fold+|fold-][ hint@<h>]
//! ```
//!
//! where the annotated line text spells out the structured-editor glyphs
//! (`▸▾│├└…→·`) and turns each space into a visible middle-dot `·`.
//!
//! **Separation of concerns:** this function is *pure text* (no styling, no
//! gpui). The Debug view's chrome/colouring is a separate UI concern handled by
//! the renderer (D2). It is intentionally `core`-local and dependency-free so it
//! can be unit-tested headlessly against the line model.
//!
//! ## Fidelity notes
//! - The C++ kept explicit `commentStart` / `typeHintStart` `int` fields on
//!   `LineMeta`; the Rust port carries these as Comment / TypeHint **chips**
//!   (`core.h` → `LineChip`), so we derive both from
//!   [`find_chip`]`(…).start_col`, defaulting to `-1` when the chip is absent —
//!   exactly the C++ sentinel.
//! - The composer emits one `\n` *between* lines (never trailing), so
//!   `text.split('\n')` is 1:1 with `meta`. We iterate `meta` and slice each
//!   line's text out of `text` by [`ComposeResult::line_starts`] (UTF-16 code
//!   units, mirroring Qt/Scintilla), matching `sciGetLineText(i)`.

use super::linemeta::{find_chip, ChipKind, ComposeResult, LineKind, LineMeta};
use crate::core::kind_to_string;

/// `lineKindNames[]` (`main.cpp:5541`) — the abbreviated `LineKind` labels used
/// only by the debug dump (note `CmdRow` / `ArrSep`, distinct from the engine's
/// full enum-variant names).
const LINE_KIND_NAMES: [&str; 7] = [
    "CmdRow", "Blank", "Header", "Field", "Cont", "Footer", "ArrSep",
];

/// The numeric `LineKind` index the C++ debug dump reads (`(int)lm->lineKind`),
/// used to index [`LINE_KIND_NAMES`]. Mirrors the `#[repr(u8)]` discriminant
/// order of [`LineKind`].
fn line_kind_index(k: LineKind) -> i32 {
    match k {
        LineKind::CommandRow => 0,
        LineKind::Blank => 1,
        LineKind::Header => 2,
        LineKind::Field => 3,
        LineKind::Continuation => 4,
        LineKind::Footer => 5,
        LineKind::ArrayElementSeparator => 6,
    }
}

/// Annotate one composed line for the debug dump — the C++ per-char switch
/// (`main.cpp:5556-5575`): spell out the structured-editor glyphs and turn each
/// space into a visible middle-dot `·`. Specifically `▸`→`[>]`, `▾`→`[v]`,
/// `│`→`[|]`, `├`→`[+]`, `└`→`[L]`, `…`→`[..]`, `→`→`[->]`, an existing margin
/// `·`→`[.]`, a plain space → `·`, everything else passthrough.
fn annotate_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len() * 2);
    for ch in line.chars() {
        match ch {
            '\u{25B8}' => out.push_str("[>]"),  // ▸ fold collapsed
            '\u{25BE}' => out.push_str("[v]"),  // ▾ fold expanded
            '\u{2502}' => out.push_str("[|]"),  // │ tree vertical
            '\u{251C}' => out.push_str("[+]"),  // ├ tree branch
            '\u{2514}' => out.push_str("[L]"),  // └ tree corner
            '\u{2026}' => out.push_str("[..]"), // … ellipsis
            '\u{2192}' => out.push_str("[->]"), // → arrow
            '\u{00B7}' => out.push_str("[.]"),  // · existing middle dot (margin)
            ' ' => out.push('\u{00B7}'),        // space → visible dot
            other => out.push(other),
        }
    }
    out
}

/// The comment-chip start column (the C++ `lm->commentStart`), or `-1`.
fn comment_start(lm: &LineMeta) -> i32 {
    find_chip(lm, ChipKind::Comment)
        .map(|c| c.start_col)
        .unwrap_or(-1)
}

/// The type-hint-chip start column (the C++ `lm->typeHintStart`), or `-1`.
fn type_hint_start(lm: &LineMeta) -> i32 {
    find_chip(lm, ChipKind::TypeHint)
        .map(|c| c.start_col)
        .unwrap_or(-1)
}

/// Slice composed line `idx`'s text out of `result.text`, the faithful
/// equivalent of `sciGetLineText(sci, i)`. `line_starts` are **UTF-16 code-unit**
/// offsets (Qt/Scintilla semantics), so we slice the UTF-16 view and rebuild a
/// `String`. The trailing `\n` is not part of any line (the composer joins, not
/// terminates), so no stripping is needed.
fn line_text(units: &[u16], starts: &[i32], idx: usize) -> String {
    let begin = starts.get(idx).copied().unwrap_or(0).max(0) as usize;
    let end = match starts.get(idx + 1) {
        // `line_starts[i+1]` points at the char *after* this line's `\n`; the
        // line's own text ends one unit earlier (the `\n` itself).
        Some(&next) => (next.max(0) as usize).saturating_sub(1),
        None => units.len(),
    };
    let begin = begin.min(units.len());
    let end = end.min(units.len());
    if end <= begin {
        return String::new();
    }
    String::from_utf16_lossy(&units[begin..end])
}

/// Generate the full debug-view text for a composed result — a faithful,
/// styling-free port of `MainWindow::generateDebugText` (`main.cpp:5534`).
///
/// Produces one output line per composed line (1:1 with `result.meta`), joined
/// by `\n`, with no trailing newline. An empty model yields the empty string.
pub fn generate_debug_text(result: &ComposeResult) -> String {
    if result.meta.is_empty() {
        return String::new();
    }

    let units: Vec<u16> = result.text.encode_utf16().collect();
    let starts = &result.line_starts;

    let mut out: Vec<String> = Vec::with_capacity(result.meta.len());
    for (i, lm) in result.meta.iter().enumerate() {
        // Margin = the line's offset text (the C++ `lm->offsetText`).
        let margin = lm.offset_text.as_str();

        let raw = line_text(&units, starts, i);
        let annotated = annotate_line(&raw);

        let lk = line_kind_index(lm.line_kind);
        let kind_name = if (0..=6).contains(&lk) {
            LINE_KIND_NAMES[lk as usize]
        } else {
            "?"
        };

        let mut meta = format!(
            "  ## L={i} {kind_name} nKind={node_kind} depth={depth} cmtStart={cmt} tW={tw} nW={nw}",
            node_kind = kind_to_string(lm.node_kind),
            depth = lm.depth,
            cmt = comment_start(lm),
            tw = lm.effective_type_w,
            nw = lm.effective_name_w,
        );
        // Flags, appended in the exact C++ order.
        if lm.is_static_line {
            meta.push_str(" static");
        }
        if lm.is_continuation {
            meta.push_str(" cont");
        }
        if lm.is_member_line {
            meta.push_str(" member");
        }
        if lm.is_array_element {
            meta.push_str(" arrElem");
        }
        if lm.fold_head {
            meta.push_str(if lm.fold_collapsed {
                " fold+"
            } else {
                " fold-"
            });
        }
        let hint = type_hint_start(lm);
        if hint >= 0 {
            meta.push_str(&format!(" hint@{hint}"));
        }

        out.push(format!("{margin}|{annotated}{meta}"));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::compose;
    use crate::core::linemeta::{LayoutInfo, LineChip};
    use crate::core::{Node, NodeKind, NodeTree};
    use crate::provider::NullProvider;

    fn child(parent: u64, kind: NodeKind, offset: i32, name: &str) -> Node {
        Node {
            parent_id: parent,
            kind,
            offset,
            name: name.to_string(),
            ..Node::default()
        }
    }

    /// Build a single-line `ComposeResult` by hand (no composer) so each
    /// formatting branch can be exercised in isolation.
    fn one_line(text: &str, lm: LineMeta) -> ComposeResult {
        ComposeResult {
            text: text.to_string(),
            meta: vec![lm],
            layout: LayoutInfo::default(),
            max_line_len: text.encode_utf16().count() as i32,
            line_starts: vec![0],
        }
    }

    #[test]
    fn empty_model_yields_empty_string() {
        let r = ComposeResult::default();
        assert_eq!(generate_debug_text(&r), "");
    }

    #[test]
    fn glyph_substitution_and_full_meta_tail() {
        // A line carrying every annotated glyph + a leading offset margin.
        // ▾ │ ├ └ … → and a literal space, plus an existing middle-dot.
        let line = "\u{25BE}\u{2502}\u{251C}\u{2514}\u{2026}\u{2192} \u{00B7}x";
        let lm = LineMeta {
            line_kind: LineKind::Field,
            node_kind: NodeKind::Int32,
            depth: 2,
            effective_type_w: 14,
            effective_name_w: 22,
            offset_text: "0010 ".to_string(),
            ..LineMeta::default()
        };
        let r = one_line(line, lm);
        let got = generate_debug_text(&r);
        // glyphs spelled out; space → ·; existing · → [.]. `kindToString`
        // returns the JSON/UI name ("Int32"), not the C type name.
        let expected = "0010 |[v][|][+][L][..][->]\u{00B7}[.]x  ## L=0 Field nKind=Int32 depth=2 cmtStart=-1 tW=14 nW=22";
        assert_eq!(got, expected);
    }

    #[test]
    fn collapsed_glyph_and_unknown_offset_margin() {
        // ▸ (collapsed) renders as [>]; empty offset margin → leading '|'.
        let lm = LineMeta {
            line_kind: LineKind::Header,
            node_kind: NodeKind::Struct,
            ..LineMeta::default()
        };
        let r = one_line("\u{25B8}Root", lm);
        let got = generate_debug_text(&r);
        assert_eq!(
            got,
            "|[>]Root  ## L=0 Header nKind=Struct depth=0 cmtStart=-1 tW=14 nW=22"
        );
    }

    #[test]
    fn flag_ordering_static_cont_member_arrelem_foldplus_hint_and_cmtstart() {
        // All flags on at once, plus a Comment chip (drives cmtStart) and a
        // TypeHint chip (drives hint@N). Asserts the EXACT C++ append order:
        // static, cont, member, arrElem, fold+, hint@N — with cmtStart inline.
        let lm = LineMeta {
            line_kind: LineKind::Continuation,
            node_kind: NodeKind::UInt32,
            depth: 3,
            effective_type_w: 20,
            effective_name_w: 30,
            is_static_line: true,
            is_continuation: true,
            is_member_line: true,
            is_array_element: true,
            fold_head: true,
            fold_collapsed: true,
            chips: vec![
                LineChip {
                    kind: ChipKind::Comment,
                    start_col: 40,
                    end_col: 50,
                    ..LineChip::default()
                },
                LineChip {
                    kind: ChipKind::TypeHint,
                    start_col: 12,
                    end_col: 18,
                    ..LineChip::default()
                },
            ],
            ..LineMeta::default()
        };
        let r = one_line("x", lm);
        let got = generate_debug_text(&r);
        assert_eq!(
            got,
            "|x  ## L=0 Cont nKind=UInt32 depth=3 cmtStart=40 tW=20 nW=30 \
             static cont member arrElem fold+ hint@12"
        );
    }

    #[test]
    fn fold_minus_branch_when_expanded() {
        // fold_head + !fold_collapsed → " fold-" (the expanded fold-head).
        let lm = LineMeta {
            line_kind: LineKind::Header,
            node_kind: NodeKind::Struct,
            fold_head: true,
            fold_collapsed: false,
            ..LineMeta::default()
        };
        let r = one_line("Root", lm);
        let got = generate_debug_text(&r);
        assert!(
            got.ends_with(" fold-"),
            "expected expanded fold-head to print ' fold-', got: {got}"
        );
        assert!(!got.contains(" fold+"));
    }

    #[test]
    fn golden_representative_structure() {
        // A representative struct: a couple of scalar fields, a nested struct,
        // an array, a pointer, and a comment — composed through the real engine,
        // then dumped. Asserts a few load-bearing line shapes (command row,
        // header fold-head, a field's nKind/offset margin, the footer) rather
        // than the whole blob, so the golden stays robust to incidental column
        // widths while still pinning the C++ format precisely.
        let mut tree = NodeTree::new();
        tree.base_address = 0;
        let ri = tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "Player".into(),
            ..Node::default()
        });
        let root_id = tree.nodes[ri].id;
        let mut f = child(root_id, NodeKind::Int32, 0, "health");
        f.comment = "hp".into();
        tree.add_node(f);
        tree.add_node(child(root_id, NodeKind::Float, 4, "speed"));
        // A nested struct.
        let ni = tree.add_node(Node {
            parent_id: root_id,
            kind: NodeKind::Struct,
            offset: 8,
            name: "pos".into(),
            ..Node::default()
        });
        let nested_id = tree.nodes[ni].id;
        tree.add_node(child(nested_id, NodeKind::Float, 0, "x"));
        tree.add_node(child(nested_id, NodeKind::Float, 4, "y"));
        // A pointer.
        tree.add_node(child(root_id, NodeKind::Pointer64, 20, "owner"));

        let prov = NullProvider;
        // show_comments=true so the comment chip drives cmtStart.
        let r = compose(
            &tree, &prov, root_id, false, false, false, false, true, true, true,
        );

        let dump = generate_debug_text(&r);
        let lines: Vec<&str> = dump.split('\n').collect();

        // 1:1 with the composed model.
        assert_eq!(lines.len(), r.meta.len());

        // Every output line is `<margin>|<text>  ## L=<i> ...` (or no `##` tail
        // only if there were a metaless line — never, since meta is 1:1 here).
        for (i, line) in lines.iter().enumerate() {
            assert!(
                line.contains(&format!("## L={i} ")),
                "line {i} missing its `## L={i}` tail: {line}"
            );
            assert!(
                line.contains('|'),
                "line {i} missing margin separator: {line}"
            );
        }

        // The command row is line 0 (CmdRow), depth 0.
        assert!(
            lines[0].contains(" CmdRow nKind="),
            "line 0 should be the CmdRow: {}",
            lines[0]
        );

        // The root header carries a fold-head marker (fold+ or fold-) and the
        // Header kind name (`kindToString` returns the JSON/UI name "Struct").
        let header = lines
            .iter()
            .find(|l| l.contains(" Header nKind=Struct "))
            .expect("a struct Header line");
        assert!(
            header.contains(" fold+") || header.contains(" fold-"),
            "header should be a fold-head: {header}"
        );

        // The commented `health` field: cmtStart must be >= 0 (a Comment chip
        // exists), and the offset margin is `0000 `.
        let health = lines
            .iter()
            .find(|l| l.contains(" Field nKind=Int32 "))
            .expect("the int `health` field line");
        assert!(
            health.starts_with("0000 |"),
            "health offset margin should be `0000 `: {health}"
        );
        let cmt = health
            .split("cmtStart=")
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .and_then(|s| s.parse::<i32>().ok())
            .expect("cmtStart token");
        assert!(
            cmt >= 0,
            "commented field should have cmtStart>=0: {health}"
        );

        // The footer line closes the struct.
        assert!(
            lines.iter().any(|l| l.contains(" Footer nKind=")),
            "a Footer line should exist in the dump:\n{dump}"
        );

        // Spaces in the body are rendered as visible middle-dots (·), never raw
        // ASCII spaces (other than inside the offset margin, which is verbatim
        // and lives before the `|`). Check a field line's post-`|` body.
        let body = health.split_once('|').unwrap().1;
        let body_before_tail = body.split("  ## ").next().unwrap();
        assert!(
            !body_before_tail.contains(' '),
            "annotated body should have no raw spaces: {body_before_tail}"
        );
    }
}
