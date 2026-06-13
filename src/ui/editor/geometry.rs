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
use crate::core::debug_view::{
    annotate_line as debug_annotate_text, line_kind_index, LINE_KIND_NAMES,
};
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
    /// Item 37: a string-kind field value (`"…"` / `L"…"`) — the C++ lexer
    /// `syntaxString` orange-tan, distinct from the green numeric value.
    StringVal,
    /// The ASCII preview column on hex rows — soft green, dim.
    Ascii,
    /// Dimmed hex bytes / fold arrows / braces / footer (`IND_HEX_DIM`).
    Dim,
    /// The root class name (`IND_CLASS_NAME`) — One Dark blue.
    ClassName,
    /// Green comment / symbol / add-comment chip (`IND_HINT_GREEN`).
    CommentGreen,
    /// The fallback type-inference chip foreground (`IND_TYPE_HINT`).
    TypeHint,
    /// An inferred type token inside a type-hint chip (`ptr64`, `uint32_t×2`).
    TypeHintType,
    /// Low-emphasis inference punctuation/operators (`[`, `]`, `,`, `|`, `->`, `✓`).
    TypeHintOperator,
    /// An inferred pointer target label/address.
    TypeHintAddress,
    /// Numeric preview inside an inference chip (`0x10`, `1.0000f`, `42`).
    TypeHintNumber,
    /// Quoted ASCII/string preview inside an inference chip.
    TypeHintString,
    /// Keyword-like preview inside an inference chip (`true`, `false`, `nullptr`).
    TypeHintKeyword,
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
    crate::core::is_func_ptr(kind)
}

/// Whether a node kind renders its type token as a keyword color (pointers and
/// `void*` lean magenta/purple like a C++ keyword in the screenshots).
pub fn is_pointer_kind(kind: crate::core::NodeKind) -> bool {
    crate::core::is_pointer_kind(kind)
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
            // chevron box (dim), source label (muted), base address (neutral),
            // the struct/class/enum keyword (dim), class name (teal), `{` dim.
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
            // Item 11: C++ explicitly applies `IND_BASE_ADDR` (= theme.text) to the
            // command-row base address, OVERRIDING the lexer's number coloring, so
            // it reads NEUTRAL (default foreground), not the orange number/address
            // hue. Body / field offset numbers keep the Address (number) role.
            push(
                &mut layers,
                compose::command_row_addr_span(text),
                SpanRole::Text,
            );
            // Keep the root type keyword (`struct`/`class`/`enum`) syntax-colored
            // so the command row still reads like code; the class name remains the
            // distinct teal `ClassName` role below.
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
                // own color (`narrowPtrValueSpan`, §8). Item 37: color the value
                // by node kind, matching the C++ per-token lexer hues:
                //   * Bool + Pointer/FnPtr (true/false/nullptr) → keyword/blue,
                //   * string kinds ("…"/L"…") → the string orange-tan role,
                //   * everything else (numbers) → green.
                use crate::core::NodeKind::*;
                let value_role = if matches!(lm.node_kind, Bool)
                    || is_pointer_kind(lm.node_kind)
                    || is_fnptr_kind(lm.node_kind)
                {
                    SpanRole::Keyword
                } else if crate::core::is_string_kind(lm.node_kind) {
                    SpanRole::StringVal
                } else {
                    SpanRole::Value
                };
                push(&mut layers, narrow_value_at_first_chip(lm, vs), value_role);
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

    // Brace dimming (trailing `{` on header lines, `braceCol`). The command row is
    // painted from the LIVE `build_command_row()` string, whose `{` sits at a
    // different column than the composed line-0 stub that `lm.brace_col` was
    // measured against (the live source / base-address / keyword / class-name
    // lengths differ from the `0x0`/`struct Untitled` placeholder). Trusting the
    // stale precomputed column dimmed a glyph INSIDE the class name (the stub
    // brace col 34 landed on `NewClass`'s 6th char). Every other command-row span
    // is re-derived from `text`, so re-scan the live text for the trailing `{`
    // here too and keep them all in lock-step.
    let brace_col = if lm.line_kind == LineKind::CommandRow {
        text.rfind('{').map(|b| col_for_byte(text, b)).unwrap_or(-1)
    } else {
        lm.brace_col
    };
    if brace_col >= 0 && brace_col < n {
        push(
            &mut layers,
            ColumnSpan {
                start: brace_col,
                end: brace_col + 1,
                valid: true,
            },
            SpanRole::Dim,
        );
    }

    // Chips (highest priority — they are tail annotations with explicit color).
    for chip in &lm.chips {
        let role = match chip.kind {
            // Item 36: the inline enum-value annotation `' (MemberName)'` is plain
            // lexer-default text in C++ (Operator/Identifier color), NOT a colored
            // link-blue pill. Paint it as the default foreground.
            ChipKind::Enum => SpanRole::Text,
            ChipKind::TypeHint => {
                let pointer_hint = chip.type_hint_kinds.iter().any(|k| {
                    matches!(
                        k,
                        crate::core::NodeKind::Pointer32 | crate::core::NodeKind::Pointer64
                    )
                });
                for span in type_hint_semantic_spans(&chip.text, pointer_hint) {
                    push_role_span(
                        &mut layers,
                        chip.start_col + span.start,
                        chip.start_col + span.end,
                        n,
                        span.role,
                    );
                }
                continue;
            }
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

    // Change heat — recolor the value-span glyphs (the orange→red hex digits in
    // PIC1/PIC5), on top of the dim hex run. `IND_HEAT_*` are TEXTFORE indicators
    // (editor-surface.md §3), so this is glyph color, not a background. Item 12:
    // the C++ `applyHeatmapHighlight` (editor.cpp:1969) fills the ENTIRE narrowed
    // value span for ANY row with heat>0 — both hex and non-hex. `changedByteIndices`
    // is used only for the dataChanged flag, NEVER for coloring; the prior per-byte
    // hex path left the unchanged bytes dim.
    if let Some(heat_role) = heat_role_for_level(lm.heat_level) {
        if !matches!(lm.line_kind, LineKind::CommandRow | LineKind::Footer) {
            let vs = narrow_value_at_first_chip(lm, compose::value_span_for(lm, type_w, name_w));
            push(&mut layers, vs, heat_role);
        }
    }

    flatten(&layers, n)
}

/// Semantic color decomposition for a type-inference chip. The result is relative
/// to `text` (start column 0) and already flattened into disjoint runs.
pub fn type_hint_semantic_spans(text: &str, pointer_hint: bool) -> Vec<SpanStyle> {
    let n = col_len(text);
    if n == 0 {
        return Vec::new();
    }

    let mut layers = vec![SpanStyle {
        start: 0,
        end: n,
        role: SpanRole::TypeHint,
    }];

    if pointer_hint {
        add_pointer_type_hint_layers(&mut layers, text, n);
    } else {
        add_preview_type_hint_layers(&mut layers, text, n);
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

fn push_role_span(layers: &mut Vec<SpanStyle>, start: i32, end: i32, n: i32, role: SpanRole) {
    let s = start.max(0).min(n);
    let e = end.max(0).min(n);
    if e > s {
        layers.push(SpanStyle {
            start: s,
            end: e,
            role,
        });
    }
}

fn push_byte_role_span(
    layers: &mut Vec<SpanStyle>,
    text: &str,
    start_byte: usize,
    end_byte: usize,
    n: i32,
    role: SpanRole,
) {
    if end_byte <= start_byte || start_byte >= text.len() {
        return;
    }
    let end_byte = end_byte.min(text.len());
    push_role_span(
        layers,
        col_for_byte(text, start_byte),
        col_for_byte(text, end_byte),
        n,
        role,
    );
}

fn add_pointer_type_hint_layers(layers: &mut Vec<SpanStyle>, text: &str, n: i32) {
    let arrow = text.find("->");
    let type_end = text
        .find('\u{2713}')
        .or(arrow)
        .or_else(|| text.find(char::is_whitespace))
        .unwrap_or(text.len());
    push_byte_role_span(layers, text, 0, type_end, n, SpanRole::TypeHintType);

    if let Some(check) = text.find('\u{2713}') {
        push_byte_role_span(
            layers,
            text,
            check,
            check + '\u{2713}'.len_utf8(),
            n,
            SpanRole::TypeHintOperator,
        );
    }

    if let Some(arrow) = arrow {
        push_byte_role_span(
            layers,
            text,
            arrow,
            arrow + "->".len(),
            n,
            SpanRole::TypeHintOperator,
        );
        let target_start = text[arrow + "->".len()..]
            .char_indices()
            .find_map(|(i, ch)| (!ch.is_whitespace()).then_some(arrow + "->".len() + i))
            .unwrap_or(text.len());
        push_byte_role_span(
            layers,
            text,
            target_start,
            text.len(),
            n,
            SpanRole::TypeHintAddress,
        );
    }
}

fn add_preview_type_hint_layers(layers: &mut Vec<SpanStyle>, text: &str, n: i32) {
    style_type_hint_preview_tokens(layers, text, 0, text.len(), n);

    let mut search_from = 0usize;
    while let Some(rel) = text[search_from..].find('[') {
        let start = search_from + rel;
        let end = text[start..].find(']').map(|i| start + i);
        push_byte_role_span(
            layers,
            text,
            start,
            start + '['.len_utf8(),
            n,
            SpanRole::TypeHintOperator,
        );
        let inner_start = start + '['.len_utf8();
        let inner_end = end.unwrap_or(text.len());
        push_byte_role_span(
            layers,
            text,
            inner_start,
            inner_end,
            n,
            SpanRole::TypeHintType,
        );
        if let Some(end) = end {
            push_byte_role_span(
                layers,
                text,
                end,
                end + ']'.len_utf8(),
                n,
                SpanRole::TypeHintOperator,
            );
            search_from = end + ']'.len_utf8();
        } else {
            break;
        }
    }

    let mut search_from = 0usize;
    while let Some(rel) = text[search_from..].find('|') {
        let at = search_from + rel;
        push_byte_role_span(
            layers,
            text,
            at,
            at + '|'.len_utf8(),
            n,
            SpanRole::TypeHintOperator,
        );
        search_from = at + '|'.len_utf8();
    }
}

fn style_type_hint_preview_tokens(
    layers: &mut Vec<SpanStyle>,
    text: &str,
    start_byte: usize,
    end_byte: usize,
    n: i32,
) {
    let mut i = start_byte.min(text.len());
    let end_byte = end_byte.min(text.len());
    while i < end_byte {
        let Some(ch) = text[i..end_byte].chars().next() else {
            break;
        };
        let ch_len = ch.len_utf8();
        if ch.is_whitespace() {
            i += ch_len;
            continue;
        }
        if matches!(ch, ',' | '|') {
            push_byte_role_span(layers, text, i, i + ch_len, n, SpanRole::TypeHintOperator);
            i += ch_len;
            continue;
        }
        if ch == '"' {
            let mut j = i + ch_len;
            while j < end_byte {
                let Some(next) = text[j..end_byte].chars().next() else {
                    break;
                };
                j += next.len_utf8();
                if next == '"' {
                    break;
                }
            }
            push_byte_role_span(layers, text, i, j, n, SpanRole::TypeHintString);
            i = j;
            continue;
        }

        let token_start = i;
        i += ch_len;
        while i < end_byte {
            let Some(next) = text[i..end_byte].chars().next() else {
                break;
            };
            if next.is_whitespace() || matches!(next, ',' | '|' | '[' | ']') {
                break;
            }
            i += next.len_utf8();
        }
        if let Some(role) = preview_token_role(&text[token_start..i]) {
            push_byte_role_span(layers, text, token_start, i, n, role);
        }
    }
}

fn preview_token_role(token: &str) -> Option<SpanRole> {
    let lower = token.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    if matches!(lower.as_str(), "true" | "false" | "nullptr") {
        return Some(SpanRole::TypeHintKeyword);
    }
    looks_like_number_preview(&lower).then_some(SpanRole::TypeHintNumber)
}

fn looks_like_number_preview(token: &str) -> bool {
    let s = token
        .trim_end_matches('f')
        .trim_start_matches(['+', '-'])
        .trim_matches('\'');
    if s.is_empty() {
        return false;
    }
    if let Some(hex) = s.strip_prefix("0x") {
        return !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit() || c == '_');
    }
    s.chars().any(|c| c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | '_' | 'e' | 'E' | '+' | '-'))
}

/// The footer pill spans (`applyCommandRowPills` / footer pill backgrounds,
/// editor-surface.md §5 step 14): the clickable add-bytes / Trim controls drawn
/// as subtle rounded chips. Each returned [`ColumnSpan`] is a char-column range to
/// paint a pill background behind. Matched longest-first so `+1000h` is not eaten
/// by `+100h`/`+10h` (the C++ collision guard). Pure (text-scan), unit-tested.
/// The byte range of composed line `idx` in `text`, derived from the UTF-16-unit
/// `line_starts` (the last / out-of-range line ends at `text.len()`). The shared
/// line-slicing idiom behind `RcxEditor::line_text`.
pub fn line_byte_range(text: &str, line_starts: &[i32], idx: usize) -> std::ops::Range<usize> {
    if idx >= line_starts.len() {
        return 0..0;
    }
    let begin = utf16_to_byte(text, line_starts[idx]);
    let end = if idx + 1 < line_starts.len() {
        utf16_to_byte(text, line_starts[idx + 1])
    } else {
        text.len()
    };
    begin..end.max(begin)
}

/// The byte range of composed line `idx` using compose-precomputed UTF-8 byte
/// starts. This is the render-path variant of [`line_byte_range`]: same output,
/// but O(1) per row instead of rescanning from the start of the full document.
pub fn line_byte_range_from_byte_starts(
    text: &str,
    line_byte_starts: &[usize],
    idx: usize,
) -> std::ops::Range<usize> {
    if idx >= line_byte_starts.len() {
        return 0..0;
    }
    let begin = line_byte_starts[idx].min(text.len());
    let end = if idx + 1 < line_byte_starts.len() {
        line_byte_starts[idx + 1].min(text.len())
    } else {
        text.len()
    };
    begin..end.max(begin)
}

/// The byte range of display span `[start, end)` in `text`. Command-row spans are
/// UTF-16-unit scans (`utf16_to_byte`); every other line's span is a display
/// column (`byte_for_col`). Shared by the inline-edit seed and keyword-hover.
pub fn span_byte_range(
    text: &str,
    start: i32,
    end: i32,
    command_row: bool,
) -> std::ops::Range<usize> {
    if command_row {
        utf16_to_byte(text, start)..utf16_to_byte(text, end)
    } else {
        byte_for_col(text, start)..byte_for_col(text, end)
    }
}

/// The footer pill (if any) whose span contains display column `col` — the
/// shared "which pill is under the cursor" scan.
pub fn footer_pill_at(text: &str, col: i32) -> Option<ColumnSpan> {
    footer_pill_spans(text)
        .into_iter()
        .find(|p| p.valid && col >= p.start && col < p.end)
}

pub fn footer_pill_spans(text: &str) -> Vec<ColumnSpan> {
    // Longest-first so a longer token's match consumes its columns before a
    // shorter token can match the suffix. `+1` (the single-field add pill) comes
    // LAST — shortest — so the claimed-columns guard blocks it from matching the
    // `+1` prefix of `+10`/`+10h`/`+100h`/`+1000h` (whose columns are already
    // claimed by the time `+1` is scanned). This mirrors the C++ footer-click
    // collision guards (editor.cpp:2592 the space-padded " +1 " probe).
    const TOKENS: [&str; 7] = ["+1000h", "+100h", "+10h", "Trim", "Top", "+10", "+1"];
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

/// Item 80 (pure): the inline LOCAL-OFFSET overlay for a child row in relative
/// mode (the C++ `IND_LOCAL_OFF` Pass 2, editor.cpp:1403). For a `Field`/`Header`
/// row at `depth > 1`, the local offset (`node_addr - parent_addr`) is rendered
/// dimly in the indent area, right-justified in the slot between the parent's and
/// the child's type column.
///
/// Returns `(start_col, slot_width, text)` for the overlay, or `None` when the row
/// does not qualify: continuation rows, non-Field/Header lines, `depth <= 1`, tree
/// lines active (the connectors own the indent), absolute mode, or — faithfully —
/// when the slot is too tight for `+XX` (`slot_width < 3`, the C++ gate; with the
/// default `K_TREE_INDENT = 2` the per-level slot is 1 char so it normally skips,
/// exactly as the C++ does). `parent_addr` is resolved by the caller (ptrBase for
/// pointer-expanded children, else `LineMeta::parent_addr`, else the view base).
pub fn local_offset_overlay(
    lm: &LineMeta,
    relative: bool,
    tree_lines: bool,
    parent_addr: u64,
) -> Option<(i32, i32, String)> {
    use crate::core::linemeta::{K_FOLD_COL, K_TREE_INDENT};
    if !relative || tree_lines || lm.is_continuation || lm.depth <= 1 {
        return None;
    }
    if lm.line_kind != LineKind::Field && lm.line_kind != LineKind::Header {
        return None;
    }
    let child_type_col = K_FOLD_COL + lm.depth * K_TREE_INDENT;
    let parent_type_col = K_FOLD_COL + (lm.depth - 1) * K_TREE_INDENT;
    let slot_width = child_type_col - parent_type_col - 1; // -1 for the gap before type
    if slot_width < 3 {
        return None; // not enough room for "+XX" (the C++ slotWidth gate)
    }
    let local_off = lm.offset_addr.saturating_sub(parent_addr);
    let off = format!("+{:X}", local_off);
    // Right-justify within the slot (truncating to the slot width if longer).
    let text: String = if (off.chars().count() as i32) <= slot_width {
        let pad = slot_width - off.chars().count() as i32;
        format!("{}{}", " ".repeat(pad.max(0) as usize), off)
    } else {
        off.chars().take(slot_width as usize).collect()
    };
    Some((parent_type_col, slot_width, text))
}

/// Item 75 (pure): the human label for a [`LineKind`] in the Debug view comes
/// from the canonical core helper (`LINE_KIND_NAMES` / `line_kind_index`, the C++
/// `lineKindNames`, main.cpp:5541); annotating a composed line (spelling out the
/// special Unicode glyphs / spaces→`·`) reuses core's `annotate_line` (re-exported
/// here as [`debug_annotate_text`]). Both live in [`crate::core::debug_view`].
///
/// Item 75 (pure): build the one-line Debug dump for line `line_idx` (the C++
/// `generateDebugText` per-line composition, main.cpp:5545). Returns
/// `margin|<annotated text>  ## L=N <LineKind> nKind=<kind> depth=N nIdx=N
/// tW=N nW=N <flags> [cmt@N] [hint@N]`. `comment_col`/`hint_col` are the comment /
/// type-hint chip start columns (Rust uses chips where the C++ had `commentStart`/
/// `typeHintStart`); pass `-1` when absent.
pub fn debug_line(
    margin: &str,
    text: &str,
    lm: &LineMeta,
    line_idx: usize,
    comment_col: i32,
    hint_col: i32,
) -> String {
    let annotated = debug_annotate_text(text);
    let mut meta = format!(
        "  ## L={} {} nKind={} depth={} nIdx={} tW={} nW={}",
        line_idx,
        LINE_KIND_NAMES[line_kind_index(lm.line_kind) as usize],
        crate::core::kind_to_string(lm.node_kind),
        lm.depth,
        lm.node_idx,
        lm.effective_type_w,
        lm.effective_name_w,
    );
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
    if comment_col >= 0 {
        meta.push_str(&format!(" cmt@{comment_col}"));
    }
    if hint_col >= 0 {
        meta.push_str(&format!(" hint@{hint_col}"));
    }
    format!("{margin}|{annotated}{meta}")
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

/// Item 2 (pure): the per-token **hover-span** column range to recolor link-blue
/// on the row under the cursor — the C++ `IND_HOVER_SPAN` pass in `applyHoverCursor`
/// (editor.cpp:4295). Returns the exact `[start,end)` char-column range of the
/// editable token / fold arrow / footer pill the cursor is over, or `None` when the
/// cursor is over column padding / inert chrome (no recolor).
///
/// The recolor span is, in `applyHoverCursor` priority order:
///   1. the fold disclosure prefix (`0..K_FOLD_COL`) on a fold-head row,
///   2. the footer pill (`+1`/`+10`/`+10h`/…/`Trim`/`Top`) under the column,
///   3. the resolved edit token under the column — narrowed at the first chip for
///      the Value column (so a pointer's trailing symbol chip keeps its own color),
/// clamped so the returned range lies inside the resolved span the cursor is on.
///
/// `hit` is the [`HitInfo`](super::hit_test::HitInfo) for the cursor column; `text`
/// is the rendered row text. Pure (column math), unit-tested below.
pub fn hover_span_for(
    lm: &LineMeta,
    text: &str,
    col: i32,
    in_fold_col: bool,
    target: Option<EditTarget>,
    type_w: i32,
    name_w: i32,
) -> Option<(i32, i32)> {
    // 1. Fold disclosure arrow — recolor the whole fold prefix.
    if in_fold_col {
        return Some((0, compose::K_FOLD_COL));
    }
    // 2. Footer pill under the cursor.
    if lm.line_kind == LineKind::Footer {
        return footer_pill_at(text, col).map(|p| (p.start, p.end));
    }
    // 3. The resolved edit token under the cursor. (Footer rows already returned;
    // the C++ explicitly skips the hover span on footer lines, editor.cpp:4296.)
    let t = target?;
    let mut span = resolved_span_for(lm, text, t, type_w, name_w);
    if t == EditTarget::Value {
        // Narrow the value at the first chip (pointer symbol / RTTI keep their hue).
        span = narrow_value_at_first_chip(lm, span);
    }
    if span.valid && col >= span.start && col < span.end && span.end > span.start {
        Some((span.start, span.end))
    } else {
        None
    }
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
        TypeSelector => compose::command_row_chevron_span(text),
    }
}

/// The `[start, end)` char-column slice of `text`, trimmed of surrounding ASCII
/// whitespace, returned with its trimmed char bounds. Mirrors the C++
/// `QString::mid(start, len).trimmed()` semantics used by the header span helpers.
fn col_slice_trimmed(text: &str, start: i32, end: i32) -> (String, i32, i32) {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len() as i32;
    let s = start.clamp(0, n);
    let e = end.clamp(s, n);
    let mut ts = s;
    while ts < e && chars[ts as usize].is_whitespace() {
        ts += 1;
    }
    let mut te = e;
    while te > ts && chars[(te - 1) as usize].is_whitespace() {
        te -= 1;
    }
    let inner: String = chars[ts as usize..te as usize].iter().collect();
    (inner, ts, te)
}

/// Header type-name span: the clickable type-name column on a struct header line
/// (NOT an array header). Faithful port of the editor-local `headerTypeNameSpan`
/// (editor.cpp:2165): rejects anonymous bare `struct`/`union`/`class` keyword
/// headers, skips a leading `static ` prefix on static-field headers, and trims
/// the column padding to the actual type-name bounds. Returns an invalid span
/// (so the caller falls through / refuses the edit) when the type column is empty
/// or a bare keyword. Items 90/91.
/// Skip leading spaces from `start`, then return the `[start, end)` column bounds
/// of the first whitespace-delimited token before `end`, or `None` if only spaces
/// remain. The shared "type column" scan in [`header_type_span`].
fn first_token_in(chars: &[char], start: i32, end: i32) -> Option<(i32, i32)> {
    let mut cursor = start;
    while cursor < end && chars[cursor as usize] == ' ' {
        cursor += 1;
    }
    let t_start = cursor;
    while cursor < end && chars[cursor as usize] != ' ' {
        cursor += 1;
    }
    if cursor > t_start {
        Some((t_start, cursor))
    } else {
        None
    }
}

/// The pure Vec/Mat value-component narrowing `begin_inline_edit` applies for a
/// Value click: split the comma-joined `raw_span`, count commas before the clicked
/// display column, and return `(component_index, seed_text)`. The index is both
/// the seeded component and the write `sub_line` (`set_node_value` routes it to
/// `addr + sub_line*4` as a Float). A single-component value seeds whole at 0; pass
/// `click_col == span_start` for a keyboard edit (no click column).
pub fn vec_component_for_click(raw_span: &str, span_start: i32, click_col: i32) -> (usize, String) {
    let comps: Vec<&str> = raw_span.split(',').collect();
    if comps.len() <= 1 {
        return (0, raw_span.trim().to_string());
    }
    let rel = (click_col - span_start).max(0) as usize;
    let span_chars: Vec<char> = raw_span.chars().collect();
    let upto = rel.min(span_chars.len());
    let comp = span_chars[..upto]
        .iter()
        .filter(|&&c| c == ',')
        .count()
        .min(comps.len() - 1);
    (comp, comps[comp].trim().to_string())
}

fn header_type_span(lm: &LineMeta, text: &str, type_w: i32) -> ColumnSpan {
    if lm.line_kind != LineKind::Header || lm.is_array_header {
        return ColumnSpan::default();
    }
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len() as i32;
    let ind = compose::K_FOLD_COL + lm.depth * compose::K_TREE_INDENT;
    let type_end = (ind + type_w).min(len);

    let (type_col, _, _) = col_slice_trimmed(text, ind, type_end);
    if type_col.is_empty() {
        return ColumnSpan::default();
    }
    // Anonymous structs use bare keywords — not clickable.
    if matches!(type_col.as_str(), "struct" | "union" | "class") {
        return ColumnSpan::default();
    }

    // Named struct: the entire type column is the type name; find its bounds
    // within the padded column.
    match first_token_in(&chars, ind, type_end) {
        Some((start, end)) => ColumnSpan {
            start,
            end,
            valid: true,
        },
        None => ColumnSpan::default(),
    }
}

/// Header field-name span: the clickable name column on a header line. Faithful
/// port of `headerNameSpan` (editor.cpp:2137): the name ends before a trailing
/// `" {"` (expanded headers) or at line end (collapsed), and the span is INVALID
/// for an empty name or an array-element name (`[0]`/`[1]`/…) — so clicking those
/// does not open an inline editor (matching C++). Items 90/91.
fn header_name_span(lm: &LineMeta, text: &str, type_w: i32, _name_w: i32) -> ColumnSpan {
    if lm.line_kind != LineKind::Header {
        return ColumnSpan::default();
    }
    let len = col_len(text);
    let ind = compose::K_FOLD_COL + lm.depth * compose::K_TREE_INDENT;
    let name_start = ind + type_w + compose::K_SEP_WIDTH;
    if name_start >= len {
        return ColumnSpan::default();
    }
    // Name ends before the " {" suffix (expanded) or at line end (collapsed).
    let mut name_end = len;
    if text.ends_with(" {") {
        name_end = len - 2;
    }
    if name_end <= name_start {
        return ColumnSpan::default();
    }
    // Reject empty / array-element names ("[0]", "[1]", …).
    let (name, _, _) = col_slice_trimmed(text, name_start, name_end);
    if name.is_empty() {
        return ColumnSpan::default();
    }
    if name.starts_with('[') && name.ends_with(']') {
        return ColumnSpan::default();
    }
    ColumnSpan {
        start: name_start,
        end: name_end,
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
    ptr_base: u64,
    hex_digits: i32,
    is_continuation: bool,
    relative: bool,
    under_ptr: bool,
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
        // Item 26: pointer-expanded children compute their RVA from the pointer's
        // target base (`lm.ptrBase`), not the struct base, so a deref'd field
        // shows the correct offset from the pointee. The C++ `reformatMargins`
        // uses `rvaBase = lm.ptrBase ? lm.ptrBase : base`.
        // Pointer-deref children measure from the pointer TARGET base — even when
        // that base is 0 (a null/unreadable target). `under_ptr` distinguishes them
        // from plain struct fields (which also have `ptr_base == 0` but measure from
        // the struct base); without it a null pointer's children underflowed to a
        // giant `+FFFF…` offset that the gutter clipped to a garbage absolute-looking
        // address.
        let rva_base = if under_ptr || ptr_base != 0 {
            ptr_base
        } else {
            base_address
        };
        let rel = offset_addr.wrapping_sub(rva_base);
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
    fn style_runs_paint_comment_chips_green() {
        let mut lm = field_line(0, NodeKind::Int32);
        let text = "   int32         health                100  // player hp";
        let comment_start = text.find("//").unwrap() as i32;
        let comment_end = col_len(text);
        lm.chips.push(crate::core::LineChip {
            kind: ChipKind::Comment,
            start_col: comment_start,
            end_col: comment_end,
            text: text[comment_start as usize..].to_string(),
            ..Default::default()
        });

        let runs = style_runs(&lm, text, 14, 22);
        assert!(runs.iter().any(|r| {
            r.role == SpanRole::CommentGreen && r.start <= comment_start && r.end >= comment_end
        }));
    }

    #[test]
    fn type_hint_semantic_spans_color_preview_parts() {
        let text = "0x6, \"AB\" [uint32_t×2] | true [bool]";
        let runs = type_hint_semantic_spans(text, false);
        assert!(
            runs.iter().any(|r| r.role == SpanRole::TypeHintNumber),
            "hex preview should get numeric hint role: {runs:?}"
        );
        assert!(
            runs.iter().any(|r| r.role == SpanRole::TypeHintString),
            "quoted ASCII preview should get string hint role: {runs:?}"
        );
        assert!(
            runs.iter().any(|r| r.role == SpanRole::TypeHintType),
            "bracketed inferred type should get type hint role: {runs:?}"
        );
        assert!(
            runs.iter()
                .filter(|r| r.role == SpanRole::TypeHintType)
                .count()
                >= 2,
            "every bracketed type in joined hints should get type role: {runs:?}"
        );
        assert!(
            runs.iter().any(|r| r.role == SpanRole::TypeHintKeyword),
            "bool/null preview should get keyword hint role: {runs:?}"
        );
        assert!(
            runs.iter().any(|r| r.role == SpanRole::TypeHintOperator),
            "brackets, comma, and pipe should get operator hint role: {runs:?}"
        );
    }

    #[test]
    fn style_runs_paint_pointer_type_hint_semantically() {
        let mut lm = field_line(0, NodeKind::Hex64);
        let prefix = "   hex64         ........              01 02 03 04 05 06 07 08  ";
        let chip_text = "ptr64\u{2713} -> 0x7FF600001000";
        let text = format!("{prefix}{chip_text}");
        let chip_start = col_len(prefix);
        lm.chips.push(crate::core::LineChip {
            kind: ChipKind::TypeHint,
            start_col: chip_start,
            end_col: chip_start + col_len(chip_text),
            text: chip_text.to_string(),
            type_hint_kinds: vec![NodeKind::Pointer64],
            ..Default::default()
        });

        let runs = style_runs(&lm, &text, 14, 22);
        assert!(
            runs.iter().any(|r| r.role == SpanRole::TypeHintType),
            "pointer type token should get type hint role: {runs:?}"
        );
        assert!(
            runs.iter().any(|r| r.role == SpanRole::TypeHintOperator),
            "pointer check/arrow should get operator hint role: {runs:?}"
        );
        assert!(
            runs.iter().any(|r| r.role == SpanRole::TypeHintAddress),
            "pointer target should get address hint role: {runs:?}"
        );
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
    fn heat_fills_whole_hex_value_span() {
        // Item 12 (corrected): the C++ `applyHeatmapHighlight` fills the ENTIRE
        // narrowed value span for ANY heated hex row — `changedByteIndices` is used
        // only for the dataChanged flag, never for coloring. (This test previously
        // asserted per-byte heat over only changed bytes; updated to match C++.)
        let mut lm = field_line(0, NodeKind::Hex64);
        lm.heat_level = 3;
        lm.changed_byte_indices = vec![0, 2];
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
        // Exactly ONE heat run, covering the whole value span (no per-byte split).
        assert_eq!(heat_runs.len(), 1, "runs={runs:?}");
        assert_eq!(heat_runs[0].start, vs.start);
        // The whole value span (all 8 byte pairs) is heated, not just bytes 0/2 —
        // so the run is far wider than the 2-col per-byte band.
        assert!(
            heat_runs[0].end - heat_runs[0].start > 6,
            "heat run should span the full value, got {:?}",
            heat_runs[0]
        );
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
    fn footer_pills_real_struct_footer_includes_single_add() {
        // The actual composed struct footer (compose::render::fmt_struct_footer):
        // `};  +1 +10h +100h +1000h Trim Top  // 0x..` — the standalone `+1`
        // single-add pill must be picked up WITHOUT colliding with the `+1`
        // prefix of `+10h`/`+100h`/`+1000h`.
        let text = "};  +1 +10h +100h +1000h Trim Top  // 0x20 (32)";
        let pills = footer_pill_spans(text);
        let chars: Vec<char> = text.chars().collect();
        let got: Vec<String> = pills
            .iter()
            .map(|s| chars[s.start as usize..s.end as usize].iter().collect())
            .collect();
        assert!(got.contains(&"+1".to_string()), "got {got:?}");
        assert!(got.contains(&"+10h".to_string()), "got {got:?}");
        assert!(got.contains(&"+100h".to_string()), "got {got:?}");
        assert!(got.contains(&"+1000h".to_string()), "got {got:?}");
        assert!(got.contains(&"Trim".to_string()), "got {got:?}");
        assert!(got.contains(&"Top".to_string()), "got {got:?}");
        // No spurious `+10` (it would be inside +10h whose cols are claimed).
        assert!(!got.contains(&"+10".to_string()), "got {got:?}");
        // 6 pills: +1, +10h, +100h, +1000h, Trim, Top.
        assert_eq!(got.len(), 6, "got {got:?}");
        for w in pills.windows(2) {
            assert!(w[0].end <= w[1].start, "pill spans must not overlap");
        }
    }

    #[test]
    fn debug_annotate_spells_glyphs_and_dots_spaces() {
        // Item 75: special glyphs are spelled out, spaces become visible dots.
        assert_eq!(debug_annotate_text("a b"), "a\u{00B7}b");
        assert_eq!(debug_annotate_text("\u{25B8}x"), "[>]x"); // ▸
        assert_eq!(debug_annotate_text("\u{25BE}"), "[v]"); // ▾
        assert_eq!(debug_annotate_text("\u{2502}\u{251C}\u{2514}"), "[|][+][L]");
        assert_eq!(debug_annotate_text("\u{2026}\u{2192}"), "[..][->]");
    }

    #[test]
    fn debug_line_dumps_margin_text_and_meta() {
        use crate::core::{LineKind, LineMeta, NodeKind};
        let lm = LineMeta {
            line_kind: LineKind::Field,
            node_kind: NodeKind::UInt32,
            depth: 2,
            node_idx: 7,
            effective_type_w: 14,
            effective_name_w: 22,
            is_continuation: false,
            fold_head: true,
            fold_collapsed: false,
            ..LineMeta::default()
        };
        let s = debug_line("+0x10", "  uint32_t health", &lm, 5, -1, 9);
        // margin | annotated-text  ## meta
        assert!(s.starts_with("+0x10|"), "got {s}");
        assert!(
            s.contains("\u{00B7}\u{00B7}uint32_t\u{00B7}health"),
            "got {s}"
        );
        assert!(
            s.contains("## L=5 Field nKind=UInt32 depth=2 nIdx=7 tW=14 nW=22"),
            "got {s}"
        );
        assert!(s.contains(" fold-"), "fold head expanded flag, got {s}");
        assert!(s.contains(" hint@9"), "type-hint column, got {s}");
        assert!(!s.contains(" cmt@"), "no comment chip → no cmt@, got {s}");
    }

    #[test]
    fn local_offset_overlay_gates_match_cpp() {
        // Item 80: the local-offset overlay only fires for Field/Header child rows
        // (depth>1) in relative mode with tree lines OFF — and faithfully skips
        // when the indent slot is too tight (`slot_width < 3`, which with the
        // default K_TREE_INDENT=2 is always the case, matching the C++ gate).
        use crate::core::{LineKind, LineMeta};
        let base = 0x1000u64;
        let child = LineMeta {
            line_kind: LineKind::Field,
            depth: 2,
            offset_addr: 0x1010,
            ..LineMeta::default()
        };
        // Absolute mode → no overlay.
        assert!(local_offset_overlay(&child, false, false, base).is_none());
        // Tree lines on → no overlay (connectors own the indent).
        assert!(local_offset_overlay(&child, true, true, base).is_none());
        // depth <= 1 → no overlay.
        let shallow = LineMeta {
            depth: 1,
            ..child.clone()
        };
        assert!(local_offset_overlay(&shallow, true, false, base).is_none());
        // Continuation row → no overlay.
        let cont = LineMeta {
            is_continuation: true,
            ..child.clone()
        };
        assert!(local_offset_overlay(&cont, true, false, base).is_none());
        // Non-Field/Header (e.g. Footer) → no overlay.
        let footer = LineMeta {
            line_kind: LineKind::Footer,
            ..child.clone()
        };
        assert!(local_offset_overlay(&footer, true, false, base).is_none());
        // A qualifying child with the default tight indent skips on the slot gate
        // (slot_width = K_TREE_INDENT-1 = 1 < 3) — faithful to the C++.
        assert!(local_offset_overlay(&child, true, false, base).is_none());
    }

    #[test]
    fn footer_pills_real_enum_footer_includes_add_ten() {
        // The composed enum footer: `};  +1 +10 Top` — here `+10` is a real pill
        // (no `+10h`), and `+1` is the single-add pill.
        let text = "};  +1 +10 Top  // 0x4 (4)";
        let pills = footer_pill_spans(text);
        let chars: Vec<char> = text.chars().collect();
        let got: Vec<String> = pills
            .iter()
            .map(|s| chars[s.start as usize..s.end as usize].iter().collect())
            .collect();
        assert!(got.contains(&"+1".to_string()), "got {got:?}");
        assert!(got.contains(&"+10".to_string()), "got {got:?}");
        assert!(got.contains(&"Top".to_string()), "got {got:?}");
        assert_eq!(got.len(), 3, "got {got:?}");
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
    fn command_row_chrome_colors_root_type_and_teals_name() {
        let lm = LineMeta {
            line_kind: LineKind::CommandRow,
            ..LineMeta::default()
        };
        let text = "[\u{25B8}] source\u{25BE}  0x400000  class Foo {";
        let runs = style_runs(&lm, text, 14, 22);
        // The root-type keyword ("class") is syntax-colored as a keyword, while
        // the root name stays teal.
        let rt = compose::command_row_root_type_span(text);
        assert!(rt.valid);
        let rt_run = runs
            .iter()
            .find(|r| r.start <= rt.start && r.end >= rt.end)
            .unwrap_or_else(|| panic!("no run covers root-type span {rt:?}: {runs:?}"));
        assert_eq!(
            rt_run.role,
            SpanRole::Keyword,
            "command-row root-type keyword must use Keyword color: {runs:?}"
        );
        // Item 11: the command-row base address is painted NEUTRAL (`SpanRole::Text`,
        // the C++ `IND_BASE_ADDR = theme.text`), OVERRIDING the orange number/address
        // hue — so there must be NO `Address` run on the command row.
        assert!(
            !runs.iter().any(|r| r.role == SpanRole::Address),
            "command-row address must be neutral Text, not Address: {runs:?}"
        );
        // The root **name** ("Foo") stays teal (`ClassName` = IND_CLASS_NAME).
        assert!(runs.iter().any(|r| r.role == SpanRole::ClassName));
    }

    #[test]
    fn value_role_splits_by_node_kind() {
        // Item 37: the value column is colored by node kind. A numeric value is
        // green (Value), a bool/pointer value is keyword/blue (true/false/nullptr).
        // Build each line so the value text actually lands in the computed value
        // span (the role only applies over `value_span_for`).
        let line_with_value = |kind: NodeKind, type_tok: &str, name: &str, value: &str| {
            let lm = field_line(0, kind);
            let vs = compose::value_span_for(&lm, 14, 22);
            let mut text = String::new();
            text.push_str(type_tok);
            while col_len(&text) < vs.start {
                text.push(' ');
            }
            text.push_str(value);
            (lm, text, name.to_string())
        };

        let (int_line, int_text, _) = line_with_value(NodeKind::Int32, "int32_t", "health", "42");
        let int_runs = style_runs(&int_line, &int_text, 14, 22);
        assert!(
            int_runs.iter().any(|r| r.role == SpanRole::Value),
            "int value should be Value (green): {int_runs:?}"
        );
        assert!(!int_runs.iter().any(|r| r.role == SpanRole::Keyword));

        let (bool_line, bool_text, _) = line_with_value(NodeKind::Bool, "bool", "flag", "true");
        let bool_runs = style_runs(&bool_line, &bool_text, 14, 22);
        assert!(
            bool_runs.iter().any(|r| r.role == SpanRole::Keyword),
            "bool value should be Keyword (blue): {bool_runs:?}"
        );

        let (ptr_line, ptr_text, _) =
            line_with_value(NodeKind::Pointer64, "ptr64", "next", "nullptr");
        let ptr_vs = compose::value_span_for(&ptr_line, 14, 22);
        let ptr_runs = style_runs(&ptr_line, &ptr_text, 14, 22);
        // A Keyword run must overlap the VALUE span (nullptr), not merely the ptr
        // type token (which is also keyword-colored).
        assert!(
            ptr_runs
                .iter()
                .any(|r| r.role == SpanRole::Keyword && r.start >= ptr_vs.start),
            "pointer value should be Keyword (blue): vs={ptr_vs:?} runs={ptr_runs:?}"
        );
    }

    #[test]
    fn enum_annotation_is_plain_text_not_chip() {
        // Item 36: the inline ' (MemberName)' enum annotation is plain default text,
        // not a colored EnumChip pill.
        let mut lm = field_line(0, NodeKind::Int32);
        let text = "int32_t       state           2 (Running)";
        // Place an Enum chip over the " (Running)" tail.
        let chip_start = text.find("(Running)").unwrap() as i32;
        lm.chips = vec![crate::core::linemeta::LineChip {
            kind: ChipKind::Enum,
            start_col: chip_start,
            end_col: chip_start + "(Running)".len() as i32,
            ..Default::default()
        }];
        let runs = style_runs(&lm, text, 14, 22);
        assert!(
            !runs.iter().any(|r| r.role == SpanRole::EnumChip),
            "enum annotation must be plain Text, not EnumChip: {runs:?}"
        );
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
        let r0 = fmt_margin_text(base, base, 0, 8, false, true, false);
        let r8 = fmt_margin_text(base + 0x8, base, 0, 8, false, true, false);
        let r10 = fmt_margin_text(base + 0x10, base, 0, 8, false, true, false);
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
        let a0 = fmt_margin_text(base, base, 0, 12, false, false, false);
        let a8 = fmt_margin_text(base + 0x8, base, 0, 12, false, false, false);
        assert_eq!(a0, "7FF60BF02B80");
        assert_eq!(a8, "7FF60BF02B88");
        assert_ne!(a0, a8);
    }

    #[test]
    fn margin_continuation_is_the_dot_marker() {
        assert_eq!(fmt_margin_text(0x40, 0, 0, 8, true, true, false), "·");
        assert_eq!(fmt_margin_text(0x40, 0, 0, 8, true, false, false), "·");
    }

    #[test]
    fn margin_empty_when_no_digits() {
        assert_eq!(fmt_margin_text(0x40, 0, 0, 0, false, true, false), "");
        assert_eq!(fmt_margin_text(0x40, 0, 0, -1, false, false, false), "");
    }

    #[test]
    fn margin_relative_uses_ptr_base_when_set() {
        // Item 26: a pointer-expanded child's RVA is computed from the pointer
        // TARGET base (`ptr_base`), not the struct base, so a deref'd field shows
        // the offset from the pointee rather than a wrong RVA off the struct base.
        let struct_base = 0x1000u64;
        let ptr_base = 0x9000u64;
        let child_addr = ptr_base + 0x18;
        // A pointer child (`under_ptr`) with a real target base → child - ptr_base.
        let with_ptr = fmt_margin_text(child_addr, struct_base, ptr_base, 8, false, true, true);
        assert!(with_ptr.trim_start().ends_with("+18"), "got {with_ptr:?}");
        // A pointer child with a NULL/unreadable target (`ptr_base == 0`): still
        // measure from the (null) target base 0, NOT the struct base — the child at
        // 0+0x18 reads "+18", not the giant underflowed offset the old code clipped
        // to a garbage absolute-looking address.
        let null_child = fmt_margin_text(0x18, struct_base, 0, 8, false, true, true);
        assert!(
            null_child.trim_start().ends_with("+18"),
            "got {null_child:?}"
        );
        // A PLAIN struct field (NOT under a pointer) with `ptr_base == 0` keeps
        // measuring from the struct base (child - base).
        let plain = fmt_margin_text(child_addr, struct_base, 0, 8, false, true, false);
        assert!(plain.trim_start().ends_with("+8018"), "got {plain:?}");
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

    // ── Item 2: per-token hover-span recolor range (the C++ `IND_HOVER_SPAN`) ──

    #[test]
    fn hover_span_recolors_the_name_token() {
        // Hovering the Name token returns exactly the Name span (link-blue recolor).
        let lm = field_line(0, NodeKind::Int32);
        let name = resolved_span_for(&lm, "", EditTarget::Name, 14, 22);
        let text = "int32         field                  100";
        let span = hover_span_for(
            &lm,
            text,
            name.start + 1,
            false,
            Some(EditTarget::Name),
            14,
            22,
        )
        .expect("hovered name token recolors");
        assert_eq!(span, (name.start, name.end));
    }

    #[test]
    fn hover_span_none_over_padding() {
        // No target under the cursor → no recolor.
        let lm = field_line(0, NodeKind::Int32);
        let text = "int32         field                  100";
        assert!(hover_span_for(&lm, text, 9, false, None, 14, 22).is_none());
    }

    #[test]
    fn hover_span_is_the_fold_prefix_on_fold_head() {
        // Item 2/3: hovering the fold arrow recolors the whole fold prefix.
        let mut lm = field_line(0, NodeKind::Struct);
        lm.line_kind = LineKind::Header;
        lm.fold_head = true;
        let text = "\u{25BE} Player                              {";
        let span = hover_span_for(&lm, text, 0, true, None, 14, 22).expect("fold arrow recolors");
        assert_eq!(span, (0, compose::K_FOLD_COL));
    }

    #[test]
    fn hover_span_is_the_footer_pill_under_cursor() {
        // Item 2/3: hovering a footer pill recolors exactly that pill's columns.
        let lm = LineMeta {
            line_kind: LineKind::Footer,
            ..LineMeta::default()
        };
        let text = "};  +1 +10h +100h +1000h Trim Top  // 0x20 (32)";
        let trim_start = text.find("Trim").unwrap() as i32;
        let span = hover_span_for(&lm, text, trim_start + 1, false, None, 14, 22)
            .expect("footer pill recolors");
        assert_eq!(span, (trim_start, trim_start + 4));
        // A non-pill footer column (the leading "};") does not recolor.
        assert!(hover_span_for(&lm, text, 0, false, None, 14, 22).is_none());
    }

    #[test]
    fn hover_span_value_narrows_at_first_chip() {
        // Item 2: the Value hover span is narrowed at the first trailing chip so a
        // pointer's symbol chip keeps its own color (mirrors `narrowPtrValueSpan`).
        let mut lm = field_line(0, NodeKind::Pointer64);
        let vs = compose::value_span_for(&lm, 14, 22);
        // Place a symbol chip a few columns into the value span.
        let chip_start = vs.start + 4;
        lm.chips.push(crate::core::LineChip {
            kind: ChipKind::Symbol,
            start_col: chip_start,
            end_col: chip_start + 6,
            ..Default::default()
        });
        let mut text = String::new();
        while col_len(&text) < vs.start {
            text.push(' ');
        }
        text.push_str("0x7ff0 kernel32");
        let span = hover_span_for(
            &lm,
            &text,
            vs.start + 1,
            false,
            Some(EditTarget::Value),
            14,
            22,
        )
        .expect("pointer value recolors");
        assert_eq!(span.0, vs.start);
        // Narrowed: the recolor ends at the chip, not the full value span.
        assert_eq!(span.1, chip_start);
    }

    // ── Header type/name span ports (items 5/90/91) ──

    fn header_line(depth: i32) -> LineMeta {
        LineMeta {
            line_kind: LineKind::Header,
            node_kind: NodeKind::Struct,
            depth,
            ..LineMeta::default()
        }
    }

    /// Build a header line text whose type column occupies `[ind, ind+type_w)` and
    /// the name follows after the separator. `ind = K_FOLD_COL + depth*K_TREE_INDENT`.
    fn compose_header_text(depth: i32, type_text: &str, type_w: i32, name: &str) -> String {
        let ind = (compose::K_FOLD_COL + depth * compose::K_TREE_INDENT) as usize;
        let mut s = " ".repeat(ind);
        s.push_str(type_text);
        // Pad the type column out to type_w.
        let cur = type_text.chars().count() as i32;
        if cur < type_w {
            s.push_str(&" ".repeat((type_w - cur) as usize));
        }
        s.push_str(&" ".repeat(compose::K_SEP_WIDTH as usize));
        s.push_str(name);
        s
    }

    #[test]
    fn header_type_span_rejects_anonymous_keyword_headers() {
        // "union {" — a bare keyword type column is NOT clickable (item 90).
        let lm = header_line(0);
        let text = compose_header_text(0, "union", 9, "{");
        let s = header_type_span(&lm, &text, 9);
        assert!(!s.valid, "bare 'union' keyword type column must be invalid");

        // "class"/"struct" likewise.
        let text2 = compose_header_text(0, "struct", 9, "{");
        assert!(!header_type_span(&lm, &text2, 9).valid);
    }

    #[test]
    fn header_type_span_trims_named_type_and_skips_brace() {
        // "_MMPTE   OriginalPte {" — the type column is just the (padded) name.
        let lm = header_line(0);
        let type_w = 12;
        let text = compose_header_text(0, "_MMPTE", type_w, "OriginalPte {");
        let s = header_type_span(&lm, &text, type_w);
        assert!(s.valid);
        let slice: String = text.chars().collect::<Vec<_>>()[s.start as usize..s.end as usize]
            .iter()
            .collect();
        // Trimmed to the actual type-name bounds (no padding, no following columns).
        assert_eq!(slice, "_MMPTE");
    }

    #[test]
    fn header_name_span_trims_trailing_brace_and_padding() {
        // Expanded header "Player   instance {" — the name ends BEFORE " {".
        let lm = header_line(0);
        let type_w = 9;
        let text = compose_header_text(0, "Player", type_w, "instance {");
        let s = header_name_span(&lm, &text, type_w, 22);
        assert!(s.valid);
        // The name span must NOT include the trailing " {".
        assert!(
            !text.chars().collect::<Vec<_>>()[s.start as usize..s.end as usize]
                .iter()
                .collect::<String>()
                .contains('{')
        );
        // And it ends exactly at len-2 (the " {" suffix).
        assert_eq!(s.end, col_len(&text) - 2);
    }

    #[test]
    fn header_name_span_rejects_array_element_names() {
        // An array-element header name "[0]" is NOT an editable name (item 91).
        let lm = header_line(0);
        let type_w = 9;
        let text = compose_header_text(0, "Player", type_w, "[0] {");
        let s = header_name_span(&lm, &text, type_w, 22);
        assert!(!s.valid, "array-element name '[0]' must be invalid");
    }
}
