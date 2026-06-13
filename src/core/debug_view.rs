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
pub(crate) const LINE_KIND_NAMES: [&str; 7] = [
    "CmdRow", "Blank", "Header", "Field", "Cont", "Footer", "ArrSep",
];

/// The numeric `LineKind` index the C++ debug dump reads (`(int)lm->lineKind`),
/// used to index [`LINE_KIND_NAMES`]. Mirrors the `#[repr(u8)]` discriminant
/// order of [`LineKind`].
pub(crate) fn line_kind_index(k: LineKind) -> i32 {
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
pub(crate) fn annotate_line(line: &str) -> String {
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

/// The nine debug-view styling classes — a 1:1 port of the style IDs the C++
/// `styleDebugText` writes into its per-byte buffer (`main.cpp:5616-5736`),
/// later mapped to `QColor`s by `applyDebugStyles` (`main.cpp:5341-5380`).
///
/// `D2` keeps the **segmentation** (which character ranges get which class) but
/// drops the per-byte Scintilla buffer: the UI maps each class to a Zed theme
/// colour and emits one styled span per range. The discriminants match the C++
/// style IDs exactly (`0=text … 8=flags`) so the mapping table stays auditable.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum DebugStyle {
    /// Style 0 — default body text (the C++ `theme.text`).
    Text = 0,
    /// Style 1 — the offset margin before the first `|` (`theme.textDim`).
    Offset = 1,
    /// Style 2 — the `|` margin separator (`theme.border.lighter(150)`).
    Pipe = 2,
    /// Style 3 — bracketed glyph markers `[>] [v] [|] [+] [L] [..] [->] [.]`
    /// (`theme.syntaxPreproc`).
    Bracket = 3,
    /// Style 4 — the visible-space middle dot `·` (`theme.textFaint`).
    MiddleDot = 4,
    /// Style 5 — the `## ` meta prefix, meta whitespace, and unknown meta tokens
    /// (`theme.syntaxComment`).
    MetaComment = 5,
    /// Style 6 — a meta key up to and including its `=` (`theme.textDim`).
    MetaKey = 6,
    /// Style 7 — a meta value after `=`, or a standalone `LineKind` token
    /// (`theme.syntaxNumber`).
    MetaValue = 7,
    /// Style 8 — a standalone flag token (`static`/`cont`/`member`/`arrElem`/
    /// `fold+`/`fold-`) (`theme.syntaxKeyword`).
    Flag = 8,
}

/// The standalone `LineKind` meta tokens (`main.cpp:5712-5714`) — styled as a
/// value ([`DebugStyle::MetaValue`]). These are the abbreviated names from
/// [`LINE_KIND_NAMES`].
const META_LINE_KINDS: [&str; 7] = [
    "CmdRow", "Blank", "Header", "Field", "Cont", "Footer", "ArrSep",
];

/// The standalone flag meta tokens (`main.cpp:5718-5719`) — styled as a keyword
/// ([`DebugStyle::Flag`]).
const META_FLAGS: [&str; 6] = ["static", "cont", "member", "arrElem", "fold+", "fold-"];

/// Faithful port of the per-line `styleDebugText` state machine
/// (`main.cpp:5616-5736`) for a **single** debug-view line, returning the style
/// segmentation as contiguous, non-overlapping `char`-index ranges that fully
/// cover the line (so the caller can slice spans 1:1). The C++ operated on a
/// UTF-8 byte buffer purely because Scintilla styles bytes; the only multi-byte
/// concern was the middle-dot `·`, which we match directly here.
///
/// The three phases mirror the original exactly:
/// 1. **Offset / pipe** — everything before the first `|` is [`Offset`], the
///    `|` itself is [`Pipe`]. With no `|`, the whole line is [`Text`].
/// 2. **Content** (after `|`, before the `  ##` marker) — `·` → [`MiddleDot`],
///    a `[..]`-style bracket run (closing `]` within 4 chars) → [`Bracket`],
///    everything else → [`Text`].
/// 3. **Meta** (from the `  ##` marker) — the `## ` prefix (4 chars) +
///    inter-token spaces → [`MetaComment`]; `key=value` → key incl. `=`
///    [`MetaKey`] / value [`MetaValue`]; a standalone `LineKind` → [`MetaValue`];
///    a standalone flag → [`Flag`]; any other standalone token → [`MetaComment`].
///
/// [`Offset`]: DebugStyle::Offset
/// [`Pipe`]: DebugStyle::Pipe
/// [`Text`]: DebugStyle::Text
/// [`MiddleDot`]: DebugStyle::MiddleDot
/// [`Bracket`]: DebugStyle::Bracket
/// [`MetaComment`]: DebugStyle::MetaComment
/// [`MetaKey`]: DebugStyle::MetaKey
/// [`MetaValue`]: DebugStyle::MetaValue
/// [`Flag`]: DebugStyle::Flag
pub fn style_debug_line(line: &str) -> Vec<(core::ops::Range<usize>, DebugStyle)> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    if n == 0 {
        return Vec::new();
    }

    // Per-char style buffer (the C++ `QByteArray styles`, but in char units),
    // default [`DebugStyle::Text`] (style 0).
    let mut styles = vec![DebugStyle::Text; n];

    // ── Phase 1: offset region (before first '|'). ──
    let pipe_pos = chars.iter().position(|&c| c == '|');

    if let Some(pipe) = pipe_pos {
        for s in styles.iter_mut().take(pipe) {
            *s = DebugStyle::Offset;
        }
        styles[pipe] = DebugStyle::Pipe;

        // ── Phase 2: content after pipe, before "  ##". ──
        // Search for the "  ##" marker (two spaces + "##"); C++ bound is
        // `lineEnd - 4`, i.e. only positions with room for all four chars.
        let mut meta_pos: Option<usize> = None;
        if n >= 4 {
            for j in (pipe + 1)..(n - 3) {
                if chars[j] == ' '
                    && chars[j + 1] == ' '
                    && chars[j + 2] == '#'
                    && chars[j + 3] == '#'
                {
                    meta_pos = Some(j);
                    break;
                }
            }
        }
        let content_end = meta_pos.unwrap_or(n);

        let mut j = pipe + 1;
        while j < content_end {
            let ch = chars[j];
            if ch == '\u{00B7}' {
                // Middle dot · (visible space).
                styles[j] = DebugStyle::MiddleDot;
            } else if ch == '[' {
                // Bracket marker: find the closing ']' within the next 4 chars
                // (the C++ `qMin(j + 5, contentEnd)` window).
                let limit = (j + 5).min(content_end);
                let mut close_b: Option<usize> = None;
                for (k, &c) in chars.iter().enumerate().take(limit).skip(j + 1) {
                    if c == ']' {
                        close_b = Some(k);
                        break;
                    }
                }
                if let Some(close) = close_b {
                    for s in styles.iter_mut().take(close + 1).skip(j) {
                        *s = DebugStyle::Bracket;
                    }
                    j = close;
                }
            }
            // else: stays DebugStyle::Text.
            j += 1;
        }

        // ── Phase 3: metadata region (from "  ##"). ──
        if let Some(meta) = meta_pos {
            // The "  ##" prefix (4 chars).
            for s in styles.iter_mut().take((meta + 4).min(n)).skip(meta) {
                *s = DebugStyle::MetaComment;
            }

            // Parse key=value pairs / keyword tokens in the meta region.
            let mut j = meta + 4;
            while j < n {
                // Skip a space (styled as meta-comment).
                if chars[j] == ' ' {
                    styles[j] = DebugStyle::MetaComment;
                    j += 1;
                    continue;
                }

                // Token spans `j..tok_end` (up to the next space).
                let mut tok_end = j;
                while tok_end < n && chars[tok_end] != ' ' {
                    tok_end += 1;
                }
                let eq_pos = (j..tok_end).find(|&k| chars[k] == '=');

                if let Some(eq) = eq_pos {
                    // Key part (through '=').
                    for s in styles.iter_mut().take(eq + 1).skip(j) {
                        *s = DebugStyle::MetaKey;
                    }
                    // Value part (after '=').
                    for s in styles.iter_mut().take(tok_end).skip(eq + 1) {
                        *s = DebugStyle::MetaValue;
                    }
                } else {
                    let tok: String = chars[j..tok_end].iter().collect();
                    let style = if META_LINE_KINDS.contains(&tok.as_str()) {
                        DebugStyle::MetaValue
                    } else if META_FLAGS.contains(&tok.as_str()) {
                        DebugStyle::Flag
                    } else {
                        DebugStyle::MetaComment
                    };
                    for s in styles.iter_mut().take(tok_end).skip(j) {
                        *s = style;
                    }
                }
                j = tok_end;
            }
        }
    }
    // else: no pipe → whole line stays DebugStyle::Text.

    // Coalesce the per-char buffer into contiguous runs.
    let mut runs: Vec<(core::ops::Range<usize>, DebugStyle)> = Vec::new();
    let mut start = 0usize;
    for k in 1..=n {
        if k == n || styles[k] != styles[start] {
            runs.push((start..k, styles[start]));
            start = k;
        }
    }
    runs
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
            line_byte_starts: vec![0],
            node_line_index: Default::default(),
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
    fn flag_ordering_cont_member_arrelem_foldplus_hint_and_cmtstart() {
        // All flags on at once, plus a Comment chip (drives cmtStart) and a
        // TypeHint chip (drives hint@N). Asserts the EXACT C++ append order:
        // cont, member, arrElem, fold+, hint@N — with cmtStart inline.
        let lm = LineMeta {
            line_kind: LineKind::Continuation,
            node_kind: NodeKind::UInt32,
            depth: 3,
            effective_type_w: 20,
            effective_name_w: 30,
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
             cont member arrElem fold+ hint@12"
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

    // ── style_debug_line (D2) ──────────────────────────────────────────────

    /// Lookup the style at a given char index from the coalesced runs.
    fn style_at(runs: &[(core::ops::Range<usize>, DebugStyle)], idx: usize) -> DebugStyle {
        runs.iter()
            .find(|(r, _)| r.contains(&idx))
            .map(|(_, s)| *s)
            .unwrap_or_else(|| panic!("no run covers char index {idx}: {runs:?}"))
    }

    /// The runs must be contiguous, non-overlapping, and cover `[0, len)`.
    fn assert_covers(runs: &[(core::ops::Range<usize>, DebugStyle)], len: usize) {
        let mut next = 0usize;
        for (r, _) in runs {
            assert_eq!(r.start, next, "gap/overlap before {r:?} in {runs:?}");
            assert!(r.end > r.start, "empty/backwards run {r:?} in {runs:?}");
            next = r.end;
        }
        assert_eq!(next, len, "runs do not cover the whole line: {runs:?}");
    }

    #[test]
    fn style_empty_line_yields_no_runs() {
        assert!(style_debug_line("").is_empty());
    }

    #[test]
    fn style_offset_and_pipe_split() {
        // "0010 |x" — offset margin (incl. trailing space) is Offset, '|' is
        // Pipe, body is Text.
        let line = "0010 |x";
        let runs = style_debug_line(line);
        assert_covers(&runs, line.chars().count());
        for i in 0..5 {
            assert_eq!(style_at(&runs, i), DebugStyle::Offset, "char {i}");
        }
        assert_eq!(style_at(&runs, 5), DebugStyle::Pipe); // '|'
        assert_eq!(style_at(&runs, 6), DebugStyle::Text); // 'x'
    }

    #[test]
    fn style_no_pipe_is_all_text() {
        // Without a '|' the whole line is the default Text style (style 0).
        let line = "no pipe here";
        let runs = style_debug_line(line);
        assert_covers(&runs, line.chars().count());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].1, DebugStyle::Text);
    }

    #[test]
    fn style_bracket_marker() {
        // "|[..]x" — the bracket run "[..]" is Bracket, trailing 'x' is Text.
        let line = "|[..]x";
        let runs = style_debug_line(line);
        assert_covers(&runs, line.chars().count());
        assert_eq!(style_at(&runs, 0), DebugStyle::Pipe);
        for i in 1..=4 {
            assert_eq!(style_at(&runs, i), DebugStyle::Bracket, "char {i} of [..]");
        }
        assert_eq!(style_at(&runs, 5), DebugStyle::Text); // 'x'
    }

    #[test]
    fn style_middle_dot() {
        // "|a\u{00B7}b" — the middle dot is MiddleDot, the letters are Text.
        let line = "|a\u{00B7}b";
        let runs = style_debug_line(line);
        assert_covers(&runs, line.chars().count());
        assert_eq!(style_at(&runs, 0), DebugStyle::Pipe);
        assert_eq!(style_at(&runs, 1), DebugStyle::Text); // 'a'
        assert_eq!(style_at(&runs, 2), DebugStyle::MiddleDot); // ·
        assert_eq!(style_at(&runs, 3), DebugStyle::Text); // 'b'
    }

    #[test]
    fn style_meta_prefix_and_key_value() {
        // The "  ## " meta prefix is MetaComment; "L=0" splits into key "L="
        // (MetaKey) and value "0" (MetaValue).
        let line = "|x  ## L=0";
        let runs = style_debug_line(line);
        assert_covers(&runs, line.chars().count());
        // "|x" = Pipe, Text.
        assert_eq!(style_at(&runs, 0), DebugStyle::Pipe);
        assert_eq!(style_at(&runs, 1), DebugStyle::Text);
        // Indices: 2=' ' 3=' ' 4='#' 5='#' — the 4-char "  ##" prefix.
        for i in 2..=5 {
            assert_eq!(
                style_at(&runs, i),
                DebugStyle::MetaComment,
                "prefix char {i}"
            );
        }
        assert_eq!(style_at(&runs, 6), DebugStyle::MetaComment); // space after ##
                                                                 // "L=" → key (incl. '='), "0" → value.
        assert_eq!(style_at(&runs, 7), DebugStyle::MetaKey); // 'L'
        assert_eq!(style_at(&runs, 8), DebugStyle::MetaKey); // '='
        assert_eq!(style_at(&runs, 9), DebugStyle::MetaValue); // '0'
    }

    #[test]
    fn style_linekind_token_is_value() {
        // A standalone LineKind token ("Field") in the meta region → MetaValue.
        let line = "|x  ## Field";
        let runs = style_debug_line(line);
        assert_covers(&runs, line.chars().count());
        // "Field" starts at index 7 (after "|x  ## ").
        for i in 7..line.chars().count() {
            assert_eq!(style_at(&runs, i), DebugStyle::MetaValue, "Field char {i}");
        }
    }

    #[test]
    fn style_flag_token_is_flag() {
        // A standalone flag token ("static") in the meta region → Flag.
        let line = "|x  ## static";
        let runs = style_debug_line(line);
        assert_covers(&runs, line.chars().count());
        // "static" starts at index 7.
        for i in 7..line.chars().count() {
            assert_eq!(style_at(&runs, i), DebugStyle::Flag, "static char {i}");
        }
    }

    #[test]
    fn style_unknown_meta_token_is_comment() {
        // An unrecognised standalone meta token (neither LineKind nor flag)
        // falls back to MetaComment (the C++ `else` branch).
        let line = "|x  ## hint@12";
        let runs = style_debug_line(line);
        assert_covers(&runs, line.chars().count());
        // "hint@12" has no '=', is not a LineKind/flag → MetaComment.
        for i in 7..line.chars().count() {
            assert_eq!(
                style_at(&runs, i),
                DebugStyle::MetaComment,
                "token char {i}"
            );
        }
    }

    #[test]
    fn style_full_line_smoke() {
        // A representative composed+annotated line exercising every phase:
        // offset margin, bracket glyph, middle dot, meta prefix, key=value,
        // LineKind value, and a flag.
        let line = "0000 |[+][->]\u{00B7}health  ## L=3 Field nKind=Int32 static";
        let runs = style_debug_line(line);
        assert_covers(&runs, line.chars().count());
        // Sanity: every style class that should appear, appears.
        let seen: std::collections::HashSet<DebugStyle> = runs.iter().map(|(_, s)| *s).collect();
        for want in [
            DebugStyle::Offset,
            DebugStyle::Pipe,
            DebugStyle::Bracket,
            DebugStyle::MiddleDot,
            DebugStyle::Text,
            DebugStyle::MetaComment,
            DebugStyle::MetaKey,
            DebugStyle::MetaValue,
            DebugStyle::Flag,
        ] {
            assert!(seen.contains(&want), "style {want:?} missing in {runs:?}");
        }
    }

    #[test]
    fn style_segmentation_aligns_with_generated_text() {
        // Cross-check: style every line of a real generated dump and assert the
        // runs cover each line exactly (no panics, full coverage) — proves the
        // segmenter stays consistent with `generate_debug_text` output.
        let mut tree = NodeTree::new();
        tree.base_address = 0;
        let ri = tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "T".into(),
            ..Node::default()
        });
        let root_id = tree.nodes[ri].id;
        tree.add_node(child(root_id, NodeKind::Int32, 0, "a"));
        tree.add_node(child(root_id, NodeKind::Float, 4, "b"));
        let prov = NullProvider;
        let r = compose(
            &tree, &prov, root_id, false, false, false, false, true, true, true,
        );
        let dump = generate_debug_text(&r);
        for line in dump.lines() {
            let runs = style_debug_line(line);
            assert_covers(&runs, line.chars().count());
        }
    }
}
