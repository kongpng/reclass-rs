//! Per-line C/C++ syntax highlighter for the Code view (extracted from tabs.rs):
//! a coarse single-pass tokenizer that classifies each run as keyword / type /
//! number / comment / string / bracket / plain and maps it to One Dark syntax
//! colors. Used by the Code tab to render highlighted source lines.

use crate::ui::design::color;
use gpui::*;

/// Coarse C/C++ token kind for the per-line highlighter.
#[derive(Copy, Clone, PartialEq, Eq)]
enum CodeTok {
    /// `struct`/`class`/`void`/`const`/`unsigned`/`#pragma`/`#include`/… — purple.
    Keyword,
    /// `uint32_t`/`int64_t`/`float`/PascalCase user types — yellow.
    Type,
    /// numeric / hex literals — orange.
    Number,
    /// `// …` trailing offset comments — dim green-gray.
    Comment,
    /// `"…"` / `<…>` string-ish runs — green.
    String,
    /// everything else (identifiers, punctuation, whitespace) — content text.
    Plain,
}

/// C/C++ keywords (highlighted purple) the generator emits.
const CPP_KEYWORDS: &[&str] = &[
    "struct",
    "class",
    "enum",
    "union",
    "void",
    "const",
    "unsigned",
    "signed",
    "static",
    "inline",
    "namespace",
    "public",
    "private",
    "protected",
    "typedef",
    "using",
    "template",
    "char",
    "bool",
    "short",
    "int",
    "long",
    "double",
    "wchar_t",
    "sizeof",
];

/// Builtin scalar type names (highlighted yellow). User struct names are caught
/// by the PascalCase / `_t`-suffix heuristic in [`classify_word`].
const CPP_TYPES: &[&str] = &[
    "uint8_t",
    "uint16_t",
    "uint32_t",
    "uint64_t",
    "int8_t",
    "int16_t",
    "int32_t",
    "int64_t",
    "__int128",
    "_Float16",
    "float",
    "size_t",
    "intptr_t",
    "uintptr_t",
];

/// Classify a single identifier-ish word for the highlighter.
fn classify_word(word: &str) -> CodeTok {
    if CPP_KEYWORDS.contains(&word) {
        return CodeTok::Keyword;
    }
    if CPP_TYPES.contains(&word) {
        return CodeTok::Type;
    }
    // Numeric / hex literal (e.g. `0x70`, `16`, `4ull`).
    let bytes = word.as_bytes();
    if bytes.first().is_some_and(u8::is_ascii_digit) {
        return CodeTok::Number;
    }
    // Heuristic "looks like a user type": leading uppercase letter or a `_t`
    // suffix (struct/class names + `*_t` aliases the user emits) → yellow.
    let first = word.chars().next();
    if first.is_some_and(|c| c.is_ascii_uppercase()) || word.ends_with("_t") {
        return CodeTok::Type;
    }
    CodeTok::Plain
}

/// Map a token kind to its One Dark syntax color (all via design tokens — no
/// ad-hoc hex). Keyword purple, type yellow, number orange, string green,
/// comment dim green-gray.
fn tok_color(tok: CodeTok, cx: &gpui::App) -> Hsla {
    match tok {
        CodeTok::Keyword => color::syntax_keyword(cx),
        CodeTok::Type => color::syntax_type(cx),
        CodeTok::Number => color::syntax_number(cx),
        CodeTok::Comment => color::syntax_comment(cx),
        CodeTok::String => color::syntax_string(cx),
        CodeTok::Plain => color::text(cx),
    }
}

/// Map a [`DebugStyle`](crate::core::DebugStyle) class to its Zed theme colour
/// (foreground only — no font-weight changes, matching the C++ `applyDebugStyles`
/// which set only `SCI_STYLESETFORE`). The C++ used a 9-entry `QColor` table
/// (`main.cpp:5359-5370`); we substitute Zed theme colours **by intent**:
///
/// | C++ style              | C++ QColor                | Zed colour            | intent  |
/// |------------------------|---------------------------|-----------------------|---------|
/// | 0 `Text`               | `theme.text`              | `text`                | normal  |
/// | 1 `Offset`             | `theme.textDim`           | `text_muted`          | DIM     |
/// | 2 `Pipe`               | `theme.border.lighter`    | `border`              | margin  |
/// | 3 `Bracket`            | `theme.syntaxPreproc`     | `accent`              | marker  |
/// | 4 `MiddleDot`          | `theme.textFaint`         | `text_disabled`       | faint   |
/// | 5 `MetaComment`        | `theme.syntaxComment`     | `syntax_comment`      | comment |
/// | 6 `MetaKey`            | `theme.textDim`           | `text_muted`          | DIM     |
/// | 7 `MetaValue`          | `theme.syntaxNumber`      | `syntax_number`       | value   |
/// | 8 `Flag`               | `theme.syntaxKeyword`     | `syntax_keyword`      | keyword |
///
/// Shared by the primary [`render_debug_view`](DocumentArea::render_debug_view)
/// and the split mirror (`window::render_split_debug`) so both agree.
pub(crate) fn debug_style_color(style: crate::core::DebugStyle, cx: &gpui::App) -> Hsla {
    use crate::core::DebugStyle as S;
    match style {
        S::Text => color::text(cx),
        S::Offset => color::text_muted(cx),
        S::Pipe => color::border(cx),
        S::Bracket => color::accent(cx),
        S::MiddleDot => color::text_disabled(cx),
        S::MetaComment => color::syntax_comment(cx),
        S::MetaKey => color::text_muted(cx),
        S::MetaValue => color::syntax_number(cx),
        S::Flag => color::syntax_keyword(cx),
    }
}

/// Build the styled spans for one debug-view line — the segmentation from
/// [`style_debug_line`](crate::core::style_debug_line) sliced into per-run
/// coloured `div`s (the gpui equivalent of the C++ per-byte Scintilla styling).
/// Each run is a `flex_none`, `whitespace_nowrap` span so the line stays on one
/// row and columns survive. Shared by the primary and split debug panes.
pub(crate) fn debug_styled_spans(line: &str, cx: &gpui::App) -> Vec<AnyElement> {
    let chars: Vec<char> = line.chars().collect();
    let runs = crate::core::style_debug_line(line);
    if runs.is_empty() {
        // A blank line: a single space keeps the row from collapsing to 0 height.
        return vec![div().flex_none().child(" ").into_any_element()];
    }
    runs.into_iter()
        .map(|(range, style)| {
            let slice: String = chars[range].iter().collect();
            div()
                .flex_none()
                .whitespace_nowrap()
                .text_color(debug_style_color(style, cx))
                .child(slice)
                .into_any_element()
        })
        .collect()
}

/// A simple per-line C/C++ tokenizer → colored spans (One Dark).
///
/// Splits a line into word / number / string / comment / punctuation runs and
/// classifies each (keyword/type/number/string/comment) so the rendered pane
/// reads like a Zed code editor. Whitespace is preserved as plain spans so
/// indentation and column alignment survive.
pub(crate) fn highlight_cpp_line(line: &str, cx: &gpui::App) -> Vec<AnyElement> {
    let mut spans: Vec<AnyElement> = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut i = 0;

    let push = |spans: &mut Vec<AnyElement>, text: String, tok: CodeTok| {
        if text.is_empty() {
            return;
        }
        spans.push(
            div()
                .flex_none()
                .whitespace_nowrap()
                .text_color(tok_color(tok, cx))
                .child(text)
                .into_any_element(),
        );
    };

    while i < n {
        let c = chars[i];

        // Trailing `// …` comment — everything to end of line (the offset notes).
        if c == '/' && i + 1 < n && chars[i + 1] == '/' {
            let rest: String = chars[i..].iter().collect();
            push(&mut spans, rest, CodeTok::Comment);
            break;
        }

        // `#pragma` / `#include` preprocessor line → keyword purple to first ws.
        if c == '#' && (i == 0 || chars[..i].iter().all(|c| c.is_whitespace())) {
            let mut j = i;
            while j < n && !chars[j].is_whitespace() {
                j += 1;
            }
            push(&mut spans, chars[i..j].iter().collect(), CodeTok::Keyword);
            i = j;
            continue;
        }

        // `"…"` string literal.
        if c == '"' {
            let mut j = i + 1;
            while j < n && chars[j] != '"' {
                j += 1;
            }
            if j < n {
                j += 1; // include closing quote
            }
            push(&mut spans, chars[i..j].iter().collect(), CodeTok::String);
            i = j;
            continue;
        }

        // `<…>` include path after a `#include` keyword → treat as a string.
        if c == '<' {
            let mut j = i + 1;
            while j < n && chars[j] != '>' {
                j += 1;
            }
            if j < n && chars[..i].iter().collect::<String>().contains('#') {
                j += 1;
                push(&mut spans, chars[i..j].iter().collect(), CodeTok::String);
                i = j;
                continue;
            }
        }

        // Whitespace run (preserved as plain).
        if c.is_whitespace() {
            let mut j = i;
            while j < n && chars[j].is_whitespace() {
                j += 1;
            }
            push(&mut spans, chars[i..j].iter().collect(), CodeTok::Plain);
            i = j;
            continue;
        }

        // Word / number run (identifier chars, plus a hex/number body).
        if c.is_alphanumeric() || c == '_' {
            let mut j = i;
            while j < n && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            let tok = classify_word(&word);
            push(&mut spans, word, tok);
            i = j;
            continue;
        }

        // Punctuation / operator run (everything else) — plain content text.
        let mut j = i;
        while j < n
            && !chars[j].is_alphanumeric()
            && chars[j] != '_'
            && !chars[j].is_whitespace()
            && chars[j] != '"'
            && !(chars[j] == '/' && j + 1 < n && chars[j + 1] == '/')
        {
            j += 1;
        }
        if j == i {
            j += 1;
        }
        push(&mut spans, chars[i..j].iter().collect(), CodeTok::Plain);
        i = j;
    }

    spans
}
