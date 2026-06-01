//! Row geometry + the row→styled-span layout (pure, headlessly testable).
//!
//! The QScintilla editor worked in **UTF-8 byte positions** and converted to/from
//! **display columns** (char indices) constantly via `SCI_FINDCOLUMN` /
//! `SCI_GETCOLUMN` (editor-surface.md §1 "Coordinate systems"). gpui's
//! `ShapedLine` indexes by **UTF-8 byte offset**, while the `compose`
//! [`ColumnSpan`](crate::compose::ColumnSpan)s are in **char (display) columns**.
//! This module owns that conversion plus the monospace x↔column mapping, and the
//! decomposition of a `LineMeta` + line text into the ordered list of colored
//! [`SpanStyle`] runs the editor paints (the Rust analogue of the Scintilla
//! indicator passes, editor-surface.md §3 & §5: hex-dim, class-name teal, chip
//! colors, tree-connector tint, comment green).
//!
//! All of this is pure (no gpui types), so it is unit-tested without a display.

use crate::compose::{self, ColumnSpan, EditTarget, LineGeometry};
use crate::core::{find_chip, is_hex_preview, ChipKind, LineKind, LineMeta};

/// A monospace cell metric: every glyph advances by `cell_width` pixels and the
/// row is `line_height` tall. The editor shapes real glyphs for painting but uses
/// this fixed grid for hit-testing math (the C++ grid is strictly monospace, so a
/// column is `floor(x / cell_width)`; editor-surface.md §8 `hitTest`).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CellMetrics {
    pub cell_width: f32,
    pub line_height: f32,
}

impl CellMetrics {
    pub fn new(cell_width: f32, line_height: f32) -> Self {
        CellMetrics {
            cell_width,
            line_height,
        }
    }

    /// Pixel X (relative to the row's left edge) of the *start* of display
    /// column `col`.
    pub fn x_for_col(&self, col: i32) -> f32 {
        col.max(0) as f32 * self.cell_width
    }

    /// Display column under pixel X (relative to row left). Rounds to the nearest
    /// cell boundary like Scintilla's `POSITIONFROMPOINTCLOSE` (the cursor snaps
    /// to the closer edge of the glyph it's over).
    pub fn col_for_x(&self, x: f32) -> i32 {
        if self.cell_width <= 0.0 {
            return 0;
        }
        (x / self.cell_width).round().max(0.0) as i32
    }

    /// Display column that *contains* pixel X (floor) — used to decide which
    /// glyph the cursor is over (chip-click / byte-click routing needs the cell
    /// the pixel falls inside, not the nearest boundary).
    pub fn col_containing_x(&self, x: f32) -> i32 {
        if self.cell_width <= 0.0 {
            return 0;
        }
        (x / self.cell_width).floor().max(0.0) as i32
    }

    /// Document line under pixel Y (relative to the content top, scroll already
    /// applied). The fallback path of `hitTest` when the click is past the text.
    pub fn line_for_y(&self, y: f32) -> i32 {
        if self.line_height <= 0.0 {
            return 0;
        }
        (y / self.line_height).floor().max(0.0) as i32
    }
}

// ── char (display column) ↔ utf-8 byte conversions (`SCI_FINDCOLUMN` / `GETCOLUMN`) ──

/// Display column → UTF-8 byte offset within `text`. A display column is a *char*
/// index (counting the multi-byte glyphs ▸▾├│└·→ as one each), so this walks
/// chars and accumulates `len_utf8`. Clamps to the end of the string.
pub fn byte_for_col(text: &str, col: i32) -> usize {
    if col <= 0 {
        return 0;
    }
    let col = col as usize;
    let mut chars = 0usize;
    for (byte_idx, _) in text.char_indices() {
        if chars == col {
            return byte_idx;
        }
        chars += 1;
    }
    text.len()
}

/// UTF-8 byte offset → display column (char index). Inverse of [`byte_for_col`].
pub fn col_for_byte(text: &str, byte: usize) -> i32 {
    let byte = byte.min(text.len());
    text[..byte].chars().count() as i32
}

/// Number of display columns (chars) in `text`.
pub fn col_len(text: &str) -> i32 {
    text.chars().count() as i32
}

/// UTF-16 code-unit offset → UTF-8 byte offset within `text`.
///
/// [`ComposeResult::line_starts`](crate::core::ComposeResult) are **UTF-16 unit**
/// offsets (the composer builds the document in a UTF-16 buffer, mirroring
/// Scintilla; editor-surface.md §1). gpui's `String`/`ShapedLine` are UTF-8, so
/// every line slice must map the UTF-16 line-start to a byte offset. Walks chars
/// accumulating both encodings; clamps to the string end.
pub fn utf16_to_byte(text: &str, utf16_units: i32) -> usize {
    if utf16_units <= 0 {
        return 0;
    }
    let target = utf16_units as usize;
    let mut units = 0usize;
    for (byte_idx, ch) in text.char_indices() {
        if units >= target {
            return byte_idx;
        }
        units += ch.len_utf16();
    }
    text.len()
}

/// A `[start,end)` byte range for a clickable/colored span, derived from a
/// char-column [`ColumnSpan`]. Returns `None` for invalid/empty spans.
pub fn byte_range_for_span(text: &str, span: ColumnSpan) -> Option<std::ops::Range<usize>> {
    if !span.valid || span.end <= span.start {
        return None;
    }
    let start = byte_for_col(text, span.start);
    let end = byte_for_col(text, span.end);
    if end <= start {
        return None;
    }
    Some(start..end)
}

// ── Row → colored span decomposition (the indicator passes, §5) ──

/// A semantic role driving a span's color, mirroring the Scintilla indicator
/// vocabulary that mattered for static rendering (editor-surface.md §3). The
/// concrete `Hsla` is resolved from the theme at paint time (so theme switches
/// are instant); this keeps the layout logic pure and testable.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SpanRole {
    /// Default text (the lexer's normal foreground).
    Text,
    /// Type-name column for value/struct kinds — One Dark yellow.
    Type,
    /// Function-pointer type token (`fnptr64`) — One Dark blue (the screenshots
    /// render function pointers distinctly from value types).
    FnPtr,
    /// `struct`/`class`/`enum`/`void*` keyword — One Dark magenta.
    Keyword,
    /// A base address / numeric literal on the command row — One Dark orange.
    Address,
    /// The command-row source label (`'Reclass.exe'`/`source`) — muted.
    Source,
    /// Field-name column.
    Name,
    /// A formatted field value (resolved addresses / numbers) — One Dark green
    /// (the `0x…` value column reads green in the reclass screenshots).
    Value,
    /// The ASCII preview column on hex rows — soft green, dim.
    Ascii,
    /// Dimmed hex bytes / fold arrows / braces / footer (`IND_HEX_DIM`).
    Dim,
    /// The root class name (`IND_CLASS_NAME`) — One Dark blue.
    ClassName,
    /// Green comment / symbol / add-comment chip (`IND_HINT_GREEN`).
    CommentGreen,
    /// The dim type-inference chip (`IND_TYPE_HINT`).
    TypeHint,
    /// The amber RTTI chip (`IND_RTTI_HINT`).
    RttiHint,
    /// The enum chip (link-blue, `IND_HOVER_SPAN`).
    EnumChip,
    /// The innermost tree connector glyph tint (`IND_TREE_CONN`).
    TreeConn,
    /// Per-byte change heat, recoloring the *glyphs* of freshly-changed bytes
    /// (editor-surface.md §3 `IND_HEAT_*` are TEXTFORE, not backgrounds). Three
    /// escalating warmth levels: cold (recently changed), warm, hot (just now).
    HeatCold,
    HeatWarm,
    HeatHot,
    /// The fold disclosure triangle (`▸`/`▾`) on an expandable row — painted crisp
    /// (a clear accent-leaning foreground), NOT dimmed like the surrounding hex,
    /// so the fold affordance reads as a real disclosure control (the task's "crisp
    /// disclosure triangle for expandable nodes" + the reclass/Zed outline look).
    FoldChevron,
}

/// Whether a node kind renders its type token as a function-pointer (One Dark
/// blue) vs a value type (yellow). Used to color the type column per kind, like
/// the reclass screenshots where `fnptr64` is blue and `hex64`/`int32` are not.
pub fn is_fnptr_kind(kind: crate::core::NodeKind) -> bool {
    use crate::core::NodeKind::*;
    matches!(kind, FuncPtr32 | FuncPtr64)
}

/// Whether a node kind renders its type token as a keyword color (pointers and
/// `void*` lean magenta/purple like a C++ keyword in the screenshots).
pub fn is_pointer_kind(kind: crate::core::NodeKind) -> bool {
    use crate::core::NodeKind::*;
    matches!(kind, Pointer32 | Pointer64)
}

/// One colored run over a `[start,end)` **char-column** range with a role.
/// Non-overlapping and ordered by `start`; the painter shapes them into
/// `TextRun`s after resolving role→color.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SpanStyle {
    pub start: i32,
    pub end: i32,
    pub role: SpanRole,
}

/// Decompose a rendered line into ordered, non-overlapping colored runs.
///
/// This is the static-coloring half of `applyDocument` (editor-surface.md §5
/// steps 11–15) expressed as pure column math: it lays a base `Text` run over the
/// whole line, then paints the higher-priority roles on top (later wins, exactly
/// like Scintilla's z-ordered indicators), and finally flattens the stack into a
/// disjoint run list. Heat / selection / hover / byte-selection are *dynamic*
/// overlays applied separately by the view; this covers the layout that depends
/// only on `LineMeta` + text.
pub fn style_runs(lm: &LineMeta, text: &str, type_w: i32, name_w: i32) -> Vec<SpanStyle> {
    let n = col_len(text);
    if n == 0 {
        return Vec::new();
    }

    // Priority-ordered stack: each later push overrides earlier ones where it
    // overlaps (z-order = push order), matching the indicator-slot composite.
    let mut layers: Vec<SpanStyle> = vec![SpanStyle {
        start: 0,
        end: n,
        role: SpanRole::Text,
    }];

    let push = |layers: &mut Vec<SpanStyle>, span: ColumnSpan, role: SpanRole| {
        if span.valid && span.end > span.start {
            layers.push(SpanStyle {
                start: span.start.max(0),
                end: span.end.min(n),
                role,
            });
        }
    };

    match lm.line_kind {
        LineKind::CommandRow => {
            // The command/header row chrome (PIC4/PIC5):
            //   "[▸] source▾  0x400000  class UnnamedClass0 {"
            // chevron box (dim), source label (muted), base address (orange),
            // the struct/class/enum keyword (magenta), class name (blue), `{` dim.
            push(
                &mut layers,
                compose::command_row_chevron_span(text),
                SpanRole::Dim,
            );
            push(
                &mut layers,
                compose::command_row_src_span(text),
                SpanRole::Source,
            );
            push(
                &mut layers,
                compose::command_row_addr_span(text),
                SpanRole::Address,
            );
            push(
                &mut layers,
                compose::command_row_root_type_span(text),
                SpanRole::Keyword,
            );
            push(
                &mut layers,
                compose::command_row_root_name_span(text),
                SpanRole::ClassName,
            );
        }
        LineKind::Footer => {
            // Whole footer is dim (`applyHexDimming`: footer text dim). The pills
            // (`+10h`/`+100h`/`+1000h`/`Trim`) get their own background quads from
            // the view; their glyphs read on the default dim text.
            layers.push(SpanStyle {
                start: 0,
                end: n,
                role: SpanRole::Dim,
            });
        }
        _ => {
            let is_hex = is_hex_preview(lm.node_kind);
            if !lm.is_member_line && !lm.is_continuation {
                // Type token, colored by node kind (item 21): function pointers
                // blue, pointers magenta-ish (keyword), NAMED struct/class types
                // teal (the C++ `syntaxType`, same teal as the root class name),
                // primitive value types blue (the C++ `syntaxKeyword`). The
                // `ClassName` role carries the teal so named composite type tokens
                // read identically to the command-row class name.
                let type_role = if is_fnptr_kind(lm.node_kind) {
                    SpanRole::FnPtr
                } else if is_pointer_kind(lm.node_kind) {
                    SpanRole::Keyword
                } else if matches!(lm.node_kind, crate::core::NodeKind::Struct) {
                    SpanRole::ClassName
                } else {
                    SpanRole::Type
                };
                push(&mut layers, compose::type_span_for(lm, type_w), type_role);
                if !is_hex {
                    // Hex rows hold the ASCII preview in the name column, not a
                    // field name (editor-surface.md §2 valueSpanFor note).
                    push(
                        &mut layers,
                        compose::name_span_for(lm, type_w, name_w),
                        SpanRole::Name,
                    );
                } else {
                    // Hex rows: the name column carries the ASCII preview
                    // (".X.%...."). Tint it the soft green ASCII role (PIC1/PIC5).
                    push(
                        &mut layers,
                        compose::name_span_for(lm, type_w, name_w),
                        SpanRole::Ascii,
                    );
                }
            }
            // Value column.
            let vs = compose::value_span_for(lm, type_w, name_w);
            if is_hex {
                // Dim the hex byte run (`IND_HEX_DIM` over the whole preview);
                // the per-byte heat overlay recolors freshly-changed bytes.
                push(&mut layers, vs, SpanRole::Dim);
            } else {
                // Clip the value at the first chip so trailing chips keep their
                // own color (`narrowPtrValueSpan`, §8).
                push(
                    &mut layers,
                    narrow_value_at_first_chip(lm, vs),
                    SpanRole::Value,
                );
            }

            // Crisp fold disclosure triangle (`▸`/`▾`): compose emits it as
            // `" ▸ "` / `" ▾ "` in the fold prefix (col 1 of the K_FOLD_COL=3
            // region) on expandable rows. Paint that single glyph in the crisp
            // `FoldChevron` role so the affordance reads as a real disclosure
            // control instead of dim chrome (the task's "crisp disclosure triangle
            // for expandable nodes"). Sits above the hex-dim/tree-connector layers.
            if lm.fold_head {
                push(
                    &mut layers,
                    ColumnSpan {
                        start: 1,
                        end: 2,
                        valid: true,
                    },
                    SpanRole::FoldChevron,
                );
            }

            // Tree-connector tint: the innermost K_TREE_INDENT chars of the indent
            // region for depth>0 rows (`applyHexDimming` tail, §5).
            if lm.depth > 0 && !matches!(lm.line_kind, LineKind::CommandRow | LineKind::Footer) {
                let ind_end = compose::K_FOLD_COL + lm.depth * compose::K_TREE_INDENT;
                let conn_start = (ind_end - compose::K_TREE_INDENT).max(0);
                push(
                    &mut layers,
                    ColumnSpan {
                        start: conn_start,
                        end: ind_end,
                        valid: true,
                    },
                    SpanRole::TreeConn,
                );
            }
        }
    }

    // Brace dimming (trailing `{` on header lines, `braceCol`).
    if lm.brace_col >= 0 && lm.brace_col < n {
        push(
            &mut layers,
            ColumnSpan {
                start: lm.brace_col,
                end: lm.brace_col + 1,
                valid: true,
            },
            SpanRole::Dim,
        );
    }

    // Chips (highest priority — they are tail annotations with explicit color).
    for chip in &lm.chips {
        let role = match chip.kind {
            ChipKind::Enum => SpanRole::EnumChip,
            ChipKind::TypeHint => SpanRole::TypeHint,
            ChipKind::Rtti => SpanRole::RttiHint,
            ChipKind::Symbol | ChipKind::Comment | ChipKind::AddComment => SpanRole::CommentGreen,
        };
        push(
            &mut layers,
            ColumnSpan {
                start: chip.start_col,
                end: chip.end_col,
                valid: true,
            },
            role,
        );
    }

    // Per-byte change heat — recolor the changed-byte *glyphs* (the orange→red
    // hex digits in PIC1/PIC5), on top of the dim hex run. `IND_HEAT_*` are
    // TEXTFORE indicators (editor-surface.md §3), so this is glyph color, not a
    // background. Hex rows recolor only `changed_byte_indices` ("XX " = 3 cols);
    // non-hex heated rows recolor the (chip-clipped) value span.
    if let Some(heat_role) = heat_role_for_level(lm.heat_level) {
        if is_hex_preview(lm.node_kind) {
            let vs = compose::value_span_for(lm, type_w, name_w);
            if vs.valid {
                for &b in &lm.changed_byte_indices {
                    let s = vs.start + b * 3;
                    push(
                        &mut layers,
                        ColumnSpan {
                            start: s,
                            end: s + 2,
                            valid: true,
                        },
                        heat_role,
                    );
                }
            }
        } else if !matches!(lm.line_kind, LineKind::CommandRow | LineKind::Footer) {
            let vs = narrow_value_at_first_chip(lm, compose::value_span_for(lm, type_w, name_w));
            push(&mut layers, vs, heat_role);
        }
    }

    flatten(&layers, n)
}

/// Map a `LineMeta::heat_level` (1=cold, 2=warm, 3=hot) to its heat [`SpanRole`],
/// or `None` for static rows (level 0).
pub fn heat_role_for_level(level: i32) -> Option<SpanRole> {
    match level {
        1 => Some(SpanRole::HeatCold),
        2 => Some(SpanRole::HeatWarm),
        3 => Some(SpanRole::HeatHot),
        _ => None,
    }
}

/// The footer pill spans (`applyCommandRowPills` / footer pill backgrounds,
/// editor-surface.md §5 step 14): the clickable add-bytes / Trim controls drawn
/// as subtle rounded chips. Each returned [`ColumnSpan`] is a char-column range to
/// paint a pill background behind. Matched longest-first so `+1000h` is not eaten
/// by `+100h`/`+10h` (the C++ collision guard). Pure (text-scan), unit-tested.
pub fn footer_pill_spans(text: &str) -> Vec<ColumnSpan> {
    // Longest-first so a longer token's match consumes its columns before a
    // shorter token can match the suffix.
    const TOKENS: [&str; 6] = ["+1000h", "+100h", "+10h", "Trim", "Top", "+10"];
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len() as i32;
    // Which columns are already claimed by a longer token.
    let mut claimed = vec![false; n.max(0) as usize];
    let mut out: Vec<ColumnSpan> = Vec::new();
    for tok in TOKENS {
        let toklen = tok.chars().count() as i32;
        if toklen == 0 || toklen > n {
            continue;
        }
        let tchars: Vec<char> = tok.chars().collect();
        let mut start = 0i32;
        while start + toklen <= n {
            let s = start as usize;
            // Match the token at `start` and ensure none of its columns are claimed.
            let mut matches = true;
            for (k, &tc) in tchars.iter().enumerate() {
                if chars[s + k] != tc || claimed[s + k] {
                    matches = false;
                    break;
                }
            }
            if matches {
                for slot in claimed.iter_mut().skip(s).take(toklen as usize) {
                    *slot = true;
                }
                out.push(ColumnSpan {
                    start,
                    end: start + toklen,
                    valid: true,
                });
                start += toklen;
            } else {
                start += 1;
            }
        }
    }
    out.sort_by_key(|s| s.start);
    out
}

/// The command-row pill spans (the chevron box + source chip): subtle rounded
/// chips the screenshots show around `[▸]` and the `source▾` selector. Returned
/// as char-column ranges for the view to paint pill backgrounds behind.
/// `source` chip spans from the source label start through the `▾` caret.
pub fn command_row_pill_spans(text: &str) -> Vec<ColumnSpan> {
    let mut out = Vec::new();
    let chevron = compose::command_row_chevron_span(text);
    if chevron.valid && chevron.end > chevron.start {
        out.push(chevron);
    }
    // Source chip: src label start .. just past the ▾ caret (one char after the
    // src span end, which lands on the arrow).
    let src = compose::command_row_src_span(text);
    if src.valid && src.end > src.start {
        out.push(ColumnSpan {
            start: src.start,
            end: src.end + 1,
            valid: true,
        });
    }
    out
}

/// Clip a value span at the first chip's `start_col` (so the editable/colored
/// value text excludes trailing RTTI/Symbol/Comment chips). Port of
/// `narrowPtrValueSpan` (editor-surface.md §8), column-only path.
pub fn narrow_value_at_first_chip(lm: &LineMeta, vs: ColumnSpan) -> ColumnSpan {
    if !vs.valid {
        return vs;
    }
    let mut end = vs.end;
    for chip in &lm.chips {
        if chip.start_col >= vs.start && chip.start_col < end {
            end = chip.start_col;
        }
    }
    ColumnSpan {
        start: vs.start,
        end,
        valid: end > vs.start,
    }
}

/// Flatten a priority-ordered (later-wins) layer stack into a disjoint,
/// `start`-sorted run list covering `[0,n)`. For every column the winning role is
/// the last layer that covers it (Scintilla composites highest indicator slot).
fn flatten(layers: &[SpanStyle], n: i32) -> Vec<SpanStyle> {
    if n <= 0 {
        return Vec::new();
    }
    // Per-column winning role (n is a small display-line width).
    let mut role_at: Vec<SpanRole> = vec![SpanRole::Text; n as usize];
    for layer in layers {
        let s = layer.start.max(0);
        let e = layer.end.min(n);
        for slot in role_at.iter_mut().take(e as usize).skip(s as usize) {
            *slot = layer.role;
        }
    }
    // Coalesce equal adjacent roles into runs.
    let mut out: Vec<SpanStyle> = Vec::new();
    let mut start = 0i32;
    for col in 1..=n {
        if col == n || role_at[col as usize] != role_at[(col - 1) as usize] {
            out.push(SpanStyle {
                start,
                end: col,
                role: role_at[(col - 1) as usize],
            });
            start = col;
        }
    }
    out
}

/// The char-column [`ColumnSpan`] for an [`EditTarget`] on a given line, mirroring
/// the editor's authoritative `resolvedSpanFor` (editor-surface.md §8) dispatch
/// onto the `compose` span helpers. Returns an invalid span when the target is
/// not editable on this line (the caller treats that as "no field here").
///
/// This is the single seam between a resolved edit target and the column math, so
/// hit-testing, tab-cycling, and edit-begin all agree on where a field lives.
pub fn resolved_span_for(
    lm: &LineMeta,
    text: &str,
    target: EditTarget,
    type_w: i32,
    name_w: i32,
) -> ColumnSpan {
    use EditTarget::*;
    let line_len = col_len(text);
    match target {
        Type => {
            if lm.line_kind == LineKind::Header {
                if lm.is_array_header {
                    // Array header Type → element-type span (picker).
                    compose::array_elem_type_span_for(lm, text)
                } else {
                    compose::command_row_root_type_span(text)
                        .valid_or(|| header_type_span(lm, text, type_w))
                }
            } else {
                compose::type_span_for(lm, type_w)
            }
        }
        Name => {
            if lm.line_kind == LineKind::Header {
                header_name_span(lm, text, type_w, name_w)
            } else if lm.is_member_line {
                compose::member_name_span_for(lm, text)
            } else if is_hex_preview(lm.node_kind) {
                ColumnSpan::default() // hex rows block Name editing (§17.2)
            } else {
                compose::name_span_for(lm, type_w, name_w)
            }
        }
        Value => {
            if lm.is_member_line {
                compose::member_value_span_for(lm, text)
            } else if is_hex_preview(lm.node_kind) {
                ColumnSpan::default() // hex value → overwrite mode, not inline edit
            } else {
                narrow_value_at_first_chip(lm, compose::value_span_for(lm, type_w, name_w))
            }
        }
        Comment => {
            // Existing comment chip span, else end-of-line fallback for creation.
            if let Some(chip) = find_chip(lm, ChipKind::Comment) {
                ColumnSpan {
                    start: chip.start_col,
                    end: chip.end_col,
                    valid: true,
                }
            } else {
                compose::comment_span_for(lm, line_len, type_w, name_w)
            }
        }
        BaseAddress => compose::command_row_addr_span(text),
        Source => compose::command_row_src_span(text),
        RootClassType => compose::command_row_root_type_span(text),
        RootClassName => compose::command_row_root_name_span(text),
        ArrayElementType => compose::array_elem_type_span_for(lm, text),
        ArrayElementCount => compose::array_elem_count_click_span_for(lm, text),
        ArrayIndex => compose::array_index_span_for(lm, text),
        ArrayCount => compose::array_count_span_for(lm, text),
        PointerTarget => compose::pointer_target_span_for(lm, text),
        StaticExpr => compose::static_expr_span_for(text),
        TypeSelector => compose::command_row_chevron_span(text),
    }
}

/// Header type-name span: the type column on a header line, rejecting anonymous
/// `struct/union/class`/`[N]` names (editor-surface.md §8 header fallbacks). A
/// minimal column-based approximation of the editor-local `headerTypeNameSpan`.
fn header_type_span(lm: &LineMeta, _text: &str, type_w: i32) -> ColumnSpan {
    let ind = compose::K_FOLD_COL + lm.depth * compose::K_TREE_INDENT;
    ColumnSpan {
        start: ind,
        end: ind + type_w,
        valid: true,
    }
}

/// Header field-name span: the name column on a header line.
fn header_name_span(lm: &LineMeta, _text: &str, type_w: i32, name_w: i32) -> ColumnSpan {
    let ind = compose::K_FOLD_COL + lm.depth * compose::K_TREE_INDENT;
    let start = ind + type_w + compose::K_SEP_WIDTH;
    ColumnSpan {
        start,
        end: start + name_w,
        valid: true,
    }
}

/// Effective type/name column widths for a line (`LineGeometry::forLine`).
pub fn effective_widths(lm: &LineMeta) -> (i32, i32) {
    let g = LineGeometry::for_line(lm);
    (g.type_column_width, g.name_column_width)
}

/// The left address/offset gutter text for a row — the Rust analogue of the C++
/// `reformatMargins` per-line text (editor-surface.md §5 `applyLineAttributes` /
/// `reformatMargins`), computed at *render* time from the row's resolved address
/// rather than trusting a precomputed string.
///
/// Two modes, matching reclass:
/// - **Relative** (`relative == true`, the default when no live memory source is
///   attached, PIC5): `"+<HEX>"` of `offset_addr - base_address`, right-justified
///   to `hex_digits` so every row's main text starts at the same column. This is
///   the reclass default the screenshots show (`+0 +8 +10 +18 …`) and avoids the
///   bug where a base-only absolute address repeats on every row.
/// - **Absolute** (`relative == false`, a live source is attached, PIC1): the full
///   uppercase hex address, zero-padded to `hex_digits` (`7FF60BF02B80 …`).
///
/// Continuation/member rows render the `"·"` middle-dot marker (the row belongs to
/// the line above). `hex_digits <= 0` yields an empty gutter.
///
/// Pure (no gpui), unit-tested below.
pub fn fmt_margin_text(
    offset_addr: u64,
    base_address: u64,
    hex_digits: i32,
    is_continuation: bool,
    relative: bool,
) -> String {
    if hex_digits <= 0 {
        return String::new();
    }
    if is_continuation {
        // The continuation marker, right-aligned in the gutter like a real row.
        return "·".to_string();
    }
    if relative {
        // Relative offset from the view base: "+<HEX>" (no 0x, uppercase), e.g.
        // `+0 +8 +10 +18`. The "+" eats one of the `hex_digits` slots so the
        // column still lines up with the absolute mode's width.
        let rel = offset_addr.wrapping_sub(base_address);
        let body = format!("{rel:X}");
        let pad = (hex_digits as usize).saturating_sub(1 + body.len());
        format!("{}+{body}", " ".repeat(pad))
    } else {
        // Absolute address, zero-padded uppercase hex (PIC1).
        let body = format!("{offset_addr:X}");
        if body.len() < hex_digits as usize {
            format!("{}{body}", "0".repeat(hex_digits as usize - body.len()))
        } else {
            body
        }
    }
}

// Small extension to chain a span fallback when invalid.
trait ColumnSpanExt {
    fn valid_or(self, f: impl FnOnce() -> ColumnSpan) -> ColumnSpan;
}
impl ColumnSpanExt for ColumnSpan {
    fn valid_or(self, f: impl FnOnce() -> ColumnSpan) -> ColumnSpan {
        if self.valid && self.end > self.start {
            self
        } else {
            f()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::NodeKind;

    fn field_line(depth: i32, kind: NodeKind) -> LineMeta {
        LineMeta {
            line_kind: LineKind::Field,
            node_kind: kind,
            depth,
            ..LineMeta::default()
        }
    }

    #[test]
    fn cell_metrics_x_and_col_round_trip() {
        let m = CellMetrics::new(8.0, 16.0);
        assert_eq!(m.x_for_col(0), 0.0);
        assert_eq!(m.x_for_col(10), 80.0);
        // Nearest-boundary snap: 79px is closer to col 10 (80) than col 9 (72).
        assert_eq!(m.col_for_x(79.0), 10);
        assert_eq!(m.col_for_x(76.0), 10); // 76 rounds to 9.5→10
        assert_eq!(m.col_for_x(75.0), 9);
        // Containing cell is a floor.
        assert_eq!(m.col_containing_x(79.0), 9);
        assert_eq!(m.col_containing_x(80.0), 10);
        assert_eq!(m.line_for_y(33.0), 2);
    }

    #[test]
    fn col_for_x_safe_when_zero_width() {
        let m = CellMetrics::new(0.0, 0.0);
        assert_eq!(m.col_for_x(100.0), 0);
        assert_eq!(m.line_for_y(100.0), 0);
    }

    #[test]
    fn ascii_col_byte_conversions_are_identity() {
        let s = "hello world";
        assert_eq!(byte_for_col(s, 0), 0);
        assert_eq!(byte_for_col(s, 5), 5);
        assert_eq!(byte_for_col(s, 100), s.len());
        assert_eq!(col_for_byte(s, 5), 5);
        assert_eq!(col_len(s), 11);
    }

    #[test]
    fn multibyte_glyphs_count_as_one_column() {
        // ▾ (U+25BE, 3 bytes) + " 0x10" — column 1 is just past the arrow.
        let s = "\u{25BE} 0x10";
        assert_eq!(col_len(s), 6);
        // Column 0 is the arrow start (byte 0); column 1 is the space (byte 3).
        assert_eq!(byte_for_col(s, 1), 3);
        assert_eq!(col_for_byte(s, 3), 1);
        // A span covering the arrow only (cols 0..1) is bytes 0..3.
        let r = byte_range_for_span(
            s,
            ColumnSpan {
                start: 0,
                end: 1,
                valid: true,
            },
        )
        .unwrap();
        assert_eq!(r, 0..3);
    }

    #[test]
    fn command_row_unit_span_seed_survives_an_astral_glyph() {
        // The command-row span helpers scan in UTF-16 units, so a span's start/end
        // are unit offsets. When a token *before* the edited span carries a non-BMP
        // glyph (an astral char counts as 2 UTF-16 units but 1 Rust char), a
        // char-based `byte_for_col` slice drifts and seeds the wrong substring —
        // `utf16_to_byte` is the correct conversion for these unit-based spans.
        // (This is the `byte_for_col`-vs-unit-span hazard called out for the
        // command-row inline-edit seed.)
        let text = "[\u{25B8}] '\u{1F4C1}data'\u{25BE}  0x0  struct Player {";
        let name = compose::command_row_root_name_span(text);
        assert!(name.valid);
        let seed_unit: String = text
            [utf16_to_byte(text, name.start)..utf16_to_byte(text, name.end)]
            .trim()
            .into();
        assert_eq!(seed_unit, "Player", "unit-based seed is the class name");
        // The char-based slice would be wrong (it lags by one unit per astral glyph).
        let seed_char: String = text[byte_for_col(text, name.start)..byte_for_col(text, name.end)]
            .trim()
            .into();
        assert_ne!(
            seed_char, "Player",
            "char-based slice drifts past the astral glyph (regression guard)"
        );
    }

    #[test]
    fn byte_range_for_invalid_span_is_none() {
        let s = "abc";
        assert_eq!(byte_range_for_span(s, ColumnSpan::default()), None);
        assert_eq!(
            byte_range_for_span(
                s,
                ColumnSpan {
                    start: 2,
                    end: 2,
                    valid: true
                }
            ),
            None
        );
    }

    #[test]
    fn style_runs_are_disjoint_sorted_and_cover_line() {
        // Build a depth-0 line whose columns match compose's geometry exactly:
        // fold(3) prefix, type[3..17], sep, name[18..40], sep, value[41..].
        use compose::{K_FOLD_COL, K_SEP_WIDTH};
        let lm = field_line(0, NodeKind::Int32);
        let type_w = 14;
        let name_w = 22;
        let value_start = (K_FOLD_COL + type_w + name_w + 2 * K_SEP_WIDTH) as usize;
        let mut text = String::new();
        text.push_str("   "); // fold prefix
        text.push_str("int32"); // type
        while col_len(&text) < (K_FOLD_COL + type_w + K_SEP_WIDTH) {
            text.push(' ');
        }
        text.push_str("field"); // name
        while col_len(&text) < value_start as i32 {
            text.push(' ');
        }
        text.push_str("100"); // value — sits exactly at value_start
        let runs = style_runs(&lm, &text, type_w, name_w);
        assert!(!runs.is_empty());
        // Sorted, contiguous, covering [0, len).
        assert_eq!(runs.first().unwrap().start, 0);
        assert_eq!(runs.last().unwrap().end, col_len(&text));
        for w in runs.windows(2) {
            assert_eq!(w[0].end, w[1].start, "runs must be contiguous");
            assert!(w[0].end > w[0].start, "runs non-empty");
        }
        // Type column colored Type; value column colored Value.
        assert!(runs.iter().any(|r| r.role == SpanRole::Type));
        assert!(runs.iter().any(|r| r.role == SpanRole::Value));
    }

    #[test]
    fn hex_row_value_column_is_dim_not_value() {
        let mut lm = field_line(0, NodeKind::Hex64);
        lm.node_kind = NodeKind::Hex64;
        let text = "hex64         AB CD EF 01 23 45 67 89   ........";
        let runs = style_runs(&lm, text, 14, 22);
        // Hex rows dim the byte run; there is no green Value role.
        assert!(runs.iter().any(|r| r.role == SpanRole::Dim));
        assert!(!runs.iter().any(|r| r.role == SpanRole::Value));
    }

    #[test]
    fn empty_line_yields_no_runs() {
        let lm = field_line(0, NodeKind::Int32);
        assert!(style_runs(&lm, "", 14, 22).is_empty());
    }

    #[test]
    fn heat_recolors_only_changed_hex_byte_glyphs() {
        // A hot hex row where bytes 0 and 2 changed this tick: only those byte
        // digit pairs get a heat role, the rest stay dim.
        let mut lm = field_line(0, NodeKind::Hex64);
        lm.heat_level = 3;
        lm.changed_byte_indices = vec![0, 2];
        // Build a line long enough to span the real value column (which starts at
        // fold(3)+type(14)+name(22)+2 seps = 41), so byte 2's digits (cols 47-49)
        // are not clipped by the line length.
        let vs = compose::value_span_for(&lm, 14, 22);
        let mut text = String::new();
        text.push_str("hex64");
        while col_len(&text) < vs.start {
            text.push(' ');
        }
        text.push_str("AB CD EF 01 23 45 67 89"); // 8 bytes ("XX " ×8 minus tail)
        let runs = style_runs(&lm, &text, 14, 22);
        let heat_runs: Vec<_> = runs
            .iter()
            .filter(|r| r.role == SpanRole::HeatHot)
            .collect();
        // Exactly two heat runs (byte 0 and byte 2), each 2 columns wide.
        assert_eq!(heat_runs.len(), 2, "runs={runs:?}");
        for r in &heat_runs {
            assert_eq!(
                r.end - r.start,
                2,
                "heat run not 2 wide: {r:?} all={heat_runs:?}"
            );
        }
        // The two heat runs sit at the right byte offsets (byte 0 and byte 2).
        assert_eq!(heat_runs[0].start, vs.start);
        assert_eq!(heat_runs[1].start, vs.start + 6);
        // The unchanged bytes keep the dim role.
        assert!(runs.iter().any(|r| r.role == SpanRole::Dim));
    }

    #[test]
    fn heat_level_zero_adds_no_heat_runs() {
        let lm = field_line(0, NodeKind::Hex64);
        let text = "hex64         AB CD EF 01 23 45 67 89   ........";
        let runs = style_runs(&lm, text, 14, 22);
        assert!(!runs.iter().any(|r| matches!(
            r.role,
            SpanRole::HeatCold | SpanRole::HeatWarm | SpanRole::HeatHot
        )));
        assert_eq!(heat_role_for_level(0), None);
        assert_eq!(heat_role_for_level(2), Some(SpanRole::HeatWarm));
    }

    #[test]
    fn narrow_value_clips_at_first_chip() {
        let mut lm = field_line(0, NodeKind::Pointer64);
        lm.chips.push(crate::core::LineChip {
            kind: ChipKind::Rtti,
            start_col: 40,
            end_col: 50,
            ..Default::default()
        });
        let vs = ColumnSpan {
            start: 30,
            end: 96,
            valid: true,
        };
        let narrowed = narrow_value_at_first_chip(&lm, vs);
        assert_eq!(narrowed.start, 30);
        assert_eq!(narrowed.end, 40);
        assert!(narrowed.valid);
    }

    #[test]
    fn resolved_span_hex_blocks_name_and_value() {
        let lm = field_line(0, NodeKind::Hex32);
        let text = "hex32         AB CD EF 01   ....";
        assert!(!resolved_span_for(&lm, text, EditTarget::Name, 14, 22).valid);
        assert!(!resolved_span_for(&lm, text, EditTarget::Value, 14, 22).valid);
        // Type is editable on a hex row.
        assert!(resolved_span_for(&lm, text, EditTarget::Type, 14, 22).valid);
    }

    #[test]
    fn footer_pills_match_longest_first_without_collision() {
        // The default footer: "};   +10h  +100h  +1000h   Trim".
        let text = "};   +10h  +100h  +1000h   Trim";
        let pills = footer_pill_spans(text);
        // Collect the matched substrings.
        let chars: Vec<char> = text.chars().collect();
        let got: Vec<String> = pills
            .iter()
            .map(|s| chars[s.start as usize..s.end as usize].iter().collect())
            .collect();
        // Each control matched exactly once; +1000h not split into +100h/+10h.
        assert!(got.contains(&"+10h".to_string()));
        assert!(got.contains(&"+100h".to_string()));
        assert!(got.contains(&"+1000h".to_string()));
        assert!(got.contains(&"Trim".to_string()));
        // Exactly four pills (no spurious +10 inside +100h/+1000h, no +100h
        // inside +1000h).
        assert_eq!(got.len(), 4, "got {got:?}");
        // Spans are sorted and non-overlapping.
        for w in pills.windows(2) {
            assert!(w[0].end <= w[1].start, "pill spans must not overlap");
        }
    }

    #[test]
    fn footer_pills_empty_when_no_controls() {
        assert!(footer_pill_spans("};").is_empty());
        assert!(footer_pill_spans("").is_empty());
    }

    #[test]
    fn command_row_pills_cover_chevron_and_source() {
        // "[▸] 'Reclass.exe'▾  0x400000  class Foo {"
        let text = "[\u{25B8}] 'Reclass.exe'\u{25BE}  0x400000  class Foo {";
        let pills = command_row_pill_spans(text);
        assert!(!pills.is_empty());
        // First pill is the chevron box at col 0.
        assert_eq!(pills[0].start, 0);
        // A source chip is present and spans past the ▾ caret.
        let src = compose::command_row_src_span(text);
        assert!(src.valid);
        assert!(pills
            .iter()
            .any(|p| p.start == src.start && p.end == src.end + 1));
    }

    #[test]
    fn fnptr_and_pointer_kind_classifiers() {
        use crate::core::NodeKind;
        assert!(is_fnptr_kind(NodeKind::FuncPtr64));
        assert!(is_fnptr_kind(NodeKind::FuncPtr32));
        assert!(!is_fnptr_kind(NodeKind::Hex64));
        assert!(is_pointer_kind(NodeKind::Pointer64));
        assert!(!is_pointer_kind(NodeKind::FuncPtr64));
    }

    #[test]
    fn command_row_chrome_colors_keyword_address_and_name() {
        let lm = LineMeta {
            line_kind: LineKind::CommandRow,
            ..LineMeta::default()
        };
        let text = "[\u{25B8}] source\u{25BE}  0x400000  class Foo {";
        let runs = style_runs(&lm, text, 14, 22);
        assert!(runs.iter().any(|r| r.role == SpanRole::Keyword));
        assert!(runs.iter().any(|r| r.role == SpanRole::Address));
        assert!(runs.iter().any(|r| r.role == SpanRole::ClassName));
    }

    #[test]
    fn fnptr_row_colors_type_token_blue() {
        let lm = field_line(0, NodeKind::FuncPtr64);
        let text = "fnptr64       event           0x7ff60baa84c0";
        let runs = style_runs(&lm, text, 14, 22);
        assert!(runs.iter().any(|r| r.role == SpanRole::FnPtr));
    }

    #[test]
    fn margin_relative_offsets_increment_per_row() {
        // PIC5: no live source → relative "+<hex>" offsets from the base. Each row
        // carries a distinct address, so the gutter must differ per row (the bug
        // was every row repeating the base address).
        let base = 0xFFFF_8000_0000_0000u64;
        let r0 = fmt_margin_text(base, base, 8, false, true);
        let r8 = fmt_margin_text(base + 0x8, base, 8, false, true);
        let r10 = fmt_margin_text(base + 0x10, base, 8, false, true);
        assert!(r0.trim_start().ends_with("+0"), "got {r0:?}");
        assert!(r8.trim_start().ends_with("+8"), "got {r8:?}");
        assert!(r10.trim_start().ends_with("+10"), "got {r10:?}");
        // Distinct per row (the regression guard).
        assert_ne!(r0, r8);
        assert_ne!(r8, r10);
        // Right-justified to the column width.
        assert_eq!(r0.chars().count(), 8);
        assert_eq!(r10.chars().count(), 8);
    }

    #[test]
    fn margin_absolute_addresses_when_source_attached() {
        // PIC1: a live source → full uppercase hex address, distinct per row.
        let base = 0x7FF6_0BF0_2B80u64;
        let a0 = fmt_margin_text(base, base, 12, false, false);
        let a8 = fmt_margin_text(base + 0x8, base, 12, false, false);
        assert_eq!(a0, "7FF60BF02B80");
        assert_eq!(a8, "7FF60BF02B88");
        assert_ne!(a0, a8);
    }

    #[test]
    fn margin_continuation_is_the_dot_marker() {
        assert_eq!(fmt_margin_text(0x40, 0, 8, true, true), "·");
        assert_eq!(fmt_margin_text(0x40, 0, 8, true, false), "·");
    }

    #[test]
    fn margin_empty_when_no_digits() {
        assert_eq!(fmt_margin_text(0x40, 0, 0, false, true), "");
        assert_eq!(fmt_margin_text(0x40, 0, -1, false, false), "");
    }

    #[test]
    fn fold_head_row_paints_crisp_chevron_role() {
        // A fold-head header row must paint the disclosure glyph (col 1) in the
        // crisp FoldChevron role, not leave it as dim chrome.
        let mut lm = field_line(0, NodeKind::Struct);
        lm.line_kind = LineKind::Header;
        lm.fold_head = true;
        lm.node_id = 3;
        // compose emits " ▾ " in the fold prefix; col 1 is the arrow.
        let text = " \u{25BE} Player                              {";
        let runs = style_runs(&lm, text, 14, 22);
        let chevron = runs
            .iter()
            .find(|r| r.role == SpanRole::FoldChevron)
            .expect("fold-head row paints a FoldChevron run");
        assert_eq!(chevron.start, 1);
        assert_eq!(chevron.end, 2);
    }

    #[test]
    fn non_fold_row_has_no_chevron_run() {
        let lm = field_line(1, NodeKind::Int32);
        let text = "   int32         field                  100";
        let runs = style_runs(&lm, text, 14, 22);
        assert!(!runs.iter().any(|r| r.role == SpanRole::FoldChevron));
    }

    #[test]
    fn resolved_span_field_name_and_value_valid() {
        let lm = field_line(1, NodeKind::Int32);
        let text = "   int32         field                  100";
        let name = resolved_span_for(&lm, text, EditTarget::Name, 14, 22);
        let value = resolved_span_for(&lm, text, EditTarget::Value, 14, 22);
        assert!(name.valid && name.end > name.start);
        assert!(value.valid && value.end > value.start);
        // Name precedes value in column order.
        assert!(name.end <= value.start);
    }
}
