//! Per-line syntax highlighter for every generated-code format. The tokenizer is
//! deliberately small, but its keyword/type/comment rules are language-aware so
//! Rust, C#, Python ctypes, and `#define` output no longer go through C++ rules.

use crate::generator::CodeFormat;
use crate::ui::design::color;
use gpui::*;

/// Coarse generated-code token kind for the per-line highlighter.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
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

/// C/C++ and `#define`-view keywords the generator emits.
const CPP_KEYWORDS: &[&str] = &[
    "struct",
    "class",
    "enum",
    "union",
    "const",
    "static",
    "inline",
    "namespace",
    "public",
    "private",
    "protected",
    "typedef",
    "using",
    "template",
    "sizeof",
];

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "union",
    "unsafe", "use", "where", "while",
];

const CSHARP_KEYWORDS: &[&str] = &[
    "abstract",
    "as",
    "base",
    "break",
    "case",
    "catch",
    "checked",
    "class",
    "const",
    "continue",
    "default",
    "delegate",
    "do",
    "else",
    "enum",
    "event",
    "explicit",
    "extern",
    "false",
    "finally",
    "fixed",
    "for",
    "foreach",
    "goto",
    "if",
    "implicit",
    "in",
    "interface",
    "internal",
    "is",
    "lock",
    "namespace",
    "new",
    "null",
    "operator",
    "out",
    "override",
    "params",
    "partial",
    "private",
    "protected",
    "public",
    "readonly",
    "ref",
    "return",
    "sealed",
    "sizeof",
    "stackalloc",
    "static",
    "struct",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "unchecked",
    "unsafe",
    "using",
    "var",
    "virtual",
    "volatile",
    "while",
];

const PYTHON_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];

/// Primitive type names across C/C++ and `#define` output. User struct names are
/// caught by the PascalCase / `_t`-suffix heuristic in [`classify_word`].
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
    "ptrdiff_t",
    "ssize_t",
    "void",
    "bool",
    "char",
    "char8_t",
    "char16_t",
    "char32_t",
    "wchar_t",
    "short",
    "int",
    "long",
    "signed",
    "unsigned",
    "double",
];

const RUST_TYPES: &[&str] = &[
    "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f32",
    "f64", "bool", "char", "str", "String",
];

const CSHARP_TYPES: &[&str] = &[
    "byte", "sbyte", "short", "ushort", "int", "uint", "long", "ulong", "nint", "nuint", "float",
    "double", "decimal", "bool", "char", "string", "object", "void",
];

const PYTHON_CTYPES: &[&str] = &[
    "c_int8",
    "c_uint8",
    "c_int16",
    "c_uint16",
    "c_int32",
    "c_uint32",
    "c_int64",
    "c_uint64",
    "c_byte",
    "c_ubyte",
    "c_short",
    "c_ushort",
    "c_int",
    "c_uint",
    "c_long",
    "c_ulong",
    "c_longlong",
    "c_ulonglong",
    "c_float",
    "c_double",
    "c_bool",
    "c_char",
    "c_wchar",
    "c_char_p",
    "c_wchar_p",
    "c_void_p",
    "c_size_t",
    "c_ssize_t",
    "Structure",
    "Union",
    "LittleEndianStructure",
    "BigEndianStructure",
    "ctypes",
    "Array",
    "POINTER",
];

fn keyword_set(format: CodeFormat) -> &'static [&'static str] {
    match format {
        CodeFormat::RustStruct => RUST_KEYWORDS,
        CodeFormat::CSharpStruct => CSHARP_KEYWORDS,
        CodeFormat::PythonCtypes => PYTHON_KEYWORDS,
        CodeFormat::CppHeader | CodeFormat::DefineOffsets => CPP_KEYWORDS,
    }
}

fn type_set(format: CodeFormat) -> &'static [&'static str] {
    match format {
        CodeFormat::RustStruct => RUST_TYPES,
        CodeFormat::CSharpStruct => CSHARP_TYPES,
        CodeFormat::PythonCtypes => PYTHON_CTYPES,
        CodeFormat::CppHeader | CodeFormat::DefineOffsets => CPP_TYPES,
    }
}

/// Classify a single identifier-ish word for the selected output language.
fn classify_word(word: &str, format: CodeFormat) -> CodeTok {
    if keyword_set(format).contains(&word) {
        return CodeTok::Keyword;
    }
    if type_set(format).contains(&word) {
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

/// Tokenize one generated source line with format-aware comment, keyword, and
/// primitive-type rules. Kept pure so every language path is regression-tested.
fn tokenize_code_line(line: &str, format: CodeFormat) -> Vec<(String, CodeTok)> {
    let mut spans = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut i = 0;

    let push = |spans: &mut Vec<(String, CodeTok)>, text: String, tok: CodeTok| {
        if text.is_empty() {
            return;
        }
        spans.push((text, tok));
    };

    while i < n {
        let c = chars[i];

        // Python uses `#` comments; the other generated languages use `//`.
        if (format == CodeFormat::PythonCtypes && c == '#')
            || (format != CodeFormat::PythonCtypes && c == '/' && i + 1 < n && chars[i + 1] == '/')
        {
            let rest: String = chars[i..].iter().collect();
            push(&mut spans, rest, CodeTok::Comment);
            break;
        }

        // C/C++ preprocessor directives and Rust attributes are keyword-colored.
        if c == '#'
            && format != CodeFormat::PythonCtypes
            && (i == 0 || chars[..i].iter().all(|c| c.is_whitespace()))
        {
            let mut j = i;
            if format == CodeFormat::RustStruct && i + 1 < n && chars[i + 1] == '[' {
                while j < n && chars[j] != ']' {
                    j += 1;
                }
                if j < n {
                    j += 1;
                }
            } else {
                while j < n && !chars[j].is_whitespace() {
                    j += 1;
                }
            }
            push(&mut spans, chars[i..j].iter().collect(), CodeTok::Keyword);
            i = j;
            continue;
        }

        // Python decorator names (`@dataclass`) use the preprocessor color.
        if format == CodeFormat::PythonCtypes && c == '@' {
            let mut j = i + 1;
            while j < n && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            push(&mut spans, chars[i..j].iter().collect(), CodeTok::Keyword);
            i = j;
            continue;
        }

        // Single- or double-quoted string/character literal, honoring escapes.
        if c == '"' || c == '\'' {
            let quote = c;
            let mut j = i + 1;
            while j < n {
                if chars[j] == '\\' {
                    j = (j + 2).min(n);
                    continue;
                }
                if chars[j] == quote {
                    j += 1;
                    break;
                }
                j += 1;
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
            let tok = classify_word(&word, format);
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
            && chars[j] != '\''
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

/// Render a format-aware token stream as inline, theme-colored GPUI spans.
pub(crate) fn highlight_code_line(
    line: &str,
    format: CodeFormat,
    cx: &gpui::App,
) -> Vec<AnyElement> {
    tokenize_code_line(line, format)
        .into_iter()
        .map(|(text, tok)| {
            div()
                .flex_none()
                .whitespace_nowrap()
                .text_color(tok_color(tok, cx))
                .child(text)
                .into_any_element()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    // Do not glob-import the parent: it imported `gpui::*`, whose `#[test]`
    // attribute would shadow Rust's built-in test harness attribute here.
    use super::{tokenize_code_line, CodeTok};
    use crate::generator::CodeFormat;

    fn kind_for(format: CodeFormat, line: &str, needle: &str) -> CodeTok {
        tokenize_code_line(line, format)
            .into_iter()
            .find_map(|(text, kind)| (text == needle).then_some(kind))
            .unwrap_or_else(|| panic!("missing token {needle:?} in {line:?}"))
    }

    #[test]
    fn highlights_cpp_keywords_types_numbers_and_comments() {
        let line = "struct Player { uint32_t hp = 0x10; }; // offset";
        assert_eq!(
            kind_for(CodeFormat::CppHeader, line, "struct"),
            CodeTok::Keyword
        );
        assert_eq!(
            kind_for(CodeFormat::CppHeader, line, "Player"),
            CodeTok::Type
        );
        assert_eq!(
            kind_for(CodeFormat::CppHeader, line, "uint32_t"),
            CodeTok::Type
        );
        assert_eq!(
            kind_for(CodeFormat::CppHeader, line, "0x10"),
            CodeTok::Number
        );
        assert_eq!(
            kind_for(CodeFormat::CppHeader, line, "// offset"),
            CodeTok::Comment
        );
    }

    #[test]
    fn highlights_rust_with_rust_rules() {
        let line = "pub struct Player { hp: u32 }";
        assert_eq!(
            kind_for(CodeFormat::RustStruct, line, "pub"),
            CodeTok::Keyword
        );
        assert_eq!(
            kind_for(CodeFormat::RustStruct, line, "struct"),
            CodeTok::Keyword
        );
        assert_eq!(
            kind_for(CodeFormat::RustStruct, line, "Player"),
            CodeTok::Type
        );
        assert_eq!(kind_for(CodeFormat::RustStruct, line, "u32"), CodeTok::Type);
    }

    #[test]
    fn highlights_define_directives_and_values() {
        let line = "#define PLAYER_HP 0x24";
        assert_eq!(
            kind_for(CodeFormat::DefineOffsets, line, "#define"),
            CodeTok::Keyword
        );
        assert_eq!(
            kind_for(CodeFormat::DefineOffsets, line, "0x24"),
            CodeTok::Number
        );
    }

    #[test]
    fn highlights_csharp_keywords_and_primitive_types() {
        let line = "public struct Player { public uint Health; }";
        assert_eq!(
            kind_for(CodeFormat::CSharpStruct, line, "public"),
            CodeTok::Keyword
        );
        assert_eq!(
            kind_for(CodeFormat::CSharpStruct, line, "struct"),
            CodeTok::Keyword
        );
        assert_eq!(
            kind_for(CodeFormat::CSharpStruct, line, "uint"),
            CodeTok::Type
        );
    }

    #[test]
    fn highlights_python_keywords_ctypes_and_hash_comments() {
        let line = "class Player(Structure): # generated";
        assert_eq!(
            kind_for(CodeFormat::PythonCtypes, line, "class"),
            CodeTok::Keyword
        );
        assert_eq!(
            kind_for(CodeFormat::PythonCtypes, line, "Player"),
            CodeTok::Type
        );
        assert_eq!(
            kind_for(CodeFormat::PythonCtypes, line, "Structure"),
            CodeTok::Type
        );
        assert_eq!(
            kind_for(CodeFormat::PythonCtypes, line, "# generated"),
            CodeTok::Comment
        );
        assert_eq!(
            kind_for(CodeFormat::PythonCtypes, "field: c_uint32", "c_uint32"),
            CodeTok::Type
        );
    }
}
