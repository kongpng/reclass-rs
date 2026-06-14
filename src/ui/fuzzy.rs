//! Fuzzy filename/symbol/command matchers — the THREE distinct scorers the C++
//! widgets used (widgets-dialogs.md §13), ported verbatim so ranking + highlight
//! positions match the originals.
//!
//! The C++ has three independent fuzzy implementations with subtly different
//! tie-breaks; the design (widgets-dialogs.md §13) flagged that the tests pin the
//! CommandPalette one strongly and the others by relative order. Rather than
//! unify on `nucleo` (and risk ordering drift the pickers' tests would catch),
//! this module reproduces each priority table exactly:
//!
//! - [`fuzzy_score`] — `rcx::fuzzyScore` (`widgets/fuzzy_match.h`), the two-pass
//!   strict matcher (contiguous substring, then word-start initials). Used by the
//!   type-selector and enum-picker popups.
//! - [`command_score`] — `CommandPalette::fuzzyScore` (`commandpalette.h:132`),
//!   the linear single-pass "Menu > Item" path scorer.
//! - [`source_score`] — `SourceChooserPopup::fuzzyScore` (`sourcechooserpopup.cpp:25`),
//!   the recursive backtracking matcher (branch cap 4) for source cards.
//!
//! Each returns a positive score on a hit (0 on miss; higher = better) and can
//! fill a `Vec<usize>` of matched character indices in the haystack (for the
//! per-character highlight painting the delegates do).
//!
//! ## Char model
//! The C++ operates on `QChar` (UTF-16 code units) and uses `toLower` /
//! `isUpper` / `isLower` / `isDigit` / `isLetter`. Reclass type/symbol/menu names
//! are ASCII, so this port operates over `Vec<char>` (Unicode scalar values) with
//! ASCII-aware classification helpers — identical behavior for every name that
//! actually flows through these widgets, and the *positions* are char indices
//! (which the GPUI highlight painter consumes the same way the Qt delegate did).
//!
//! Gated behind the `ui` feature (these back the UI pickers); pure + headless, so
//! the whole module is exercised by `#[cfg(test)]` against the C++ test-locked
//! cases (`test_command_palette.cpp`, `test_type_selector.cpp`).

/// `kMaxFuzzyLen` — the pattern-length cap shared by `rcx::fuzzyScore` and the
/// source scorer (`fuzzy_match.h:32`, `sourcechooserpopup.cpp:22`).
pub const MAX_FUZZY_LEN: usize = 64;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceScorePattern {
    chars: Vec<char>,
    lower: Vec<char>,
}

impl SourceScorePattern {
    pub fn new(pattern: &str) -> Self {
        let chars: Vec<char> = pattern.chars().collect();
        let lower = chars.iter().map(|&c| lower(c)).collect();
        Self { chars, lower }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceScoreText {
    chars: Vec<char>,
    lower: Vec<char>,
}

impl SourceScoreText {
    pub fn new(text: &str) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let lower = chars.iter().map(|&c| lower(c)).collect();
        Self { chars, lower }
    }
}

// ── ASCII char classification (the QChar predicates the C++ used) ──

#[inline]
fn lower(c: char) -> char {
    c.to_ascii_lowercase()
}
#[inline]
fn is_upper(c: char) -> bool {
    c.is_ascii_uppercase()
}
#[inline]
fn is_lower(c: char) -> bool {
    c.is_ascii_lowercase()
}
#[inline]
fn is_digit(c: char) -> bool {
    c.is_ascii_digit()
}
#[inline]
fn is_letter(c: char) -> bool {
    c.is_ascii_alphabetic()
}

/// Case-insensitive "does `text` contain `pattern`": returns the start char
/// index of the first occurrence, mirroring `QString::indexOf(.., CaseInsensitive)`.
fn index_of_ci(text: &[char], pattern: &[char]) -> Option<usize> {
    if pattern.is_empty() {
        return Some(0);
    }
    if pattern.len() > text.len() {
        return None;
    }
    'outer: for start in 0..=(text.len() - pattern.len()) {
        for (i, &pc) in pattern.iter().enumerate() {
            if lower(text[start + i]) != lower(pc) {
                continue 'outer;
            }
        }
        return Some(start);
    }
    None
}

/// `rcx::fuzzyScore` (`widgets/fuzzy_match.h:34`) — the two-pass strict matcher
/// used by the type-selector and enum-picker popups.
///
/// Pass 1 is a contiguous case-insensitive substring (the common case, highest
/// score); pass 2 falls back to word-start initials (acronyms like `GPA` →
/// `GetProcAddress`, `u32` → `uint32_t`), rejecting scattered-subsequence noise.
/// Returns `1` for an empty pattern, `0` when `pattern` is longer than `text` or
/// `text` exceeds 4096 chars, else the tiered score. `out_positions`, when
/// `Some`, receives the matched char indices in `text`.
pub fn fuzzy_score(pattern: &str, text: &str, out_positions: Option<&mut Vec<usize>>) -> i32 {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let p_len = p.len();
    let t_len = t.len();

    if p_len == 0 {
        return 1;
    }
    if p_len > t_len {
        return 0;
    }
    if t_len > 4096 {
        return 0;
    }

    // ── Pass 1: contiguous substring (case-insensitive) ──
    if let Some(idx) = index_of_ci(&t, &p) {
        if let Some(out) = out_positions {
            out.clear();
            out.extend(idx..idx + p_len);
        }
        // prefix-of-text (idx==0): +500; after _/./:/- separator: +200;
        // CamelCase boundary: +150; word-internal: base only. Plus tightness +
        // exact-length bonus.
        let mut score = 1000;
        if idx == 0 {
            score += 500;
        } else {
            let prev = t[idx - 1];
            if prev == '_' || prev == ' ' || prev == ':' || prev == '.' || prev == '-' {
                score += 200;
            } else if is_upper(t[idx]) && is_lower(prev) {
                score += 150;
            }
        }
        score += (100 - (t_len as i32 - p_len as i32)).max(0);
        if p_len == t_len {
            score += 200;
        }
        return score;
    }

    // ── Pass 2: word-start initials ──
    let matchable = |i: usize| -> bool {
        if i == 0 {
            return true;
        }
        let c = t[i];
        let pch = t[i - 1];
        if pch == '_' || pch == ' ' || pch == ':' || pch == '.' || pch == '-' {
            return true;
        }
        if is_upper(c) && is_lower(pch) {
            return true;
        }
        if is_digit(c) && is_letter(pch) {
            return true;
        }
        if is_letter(c) && is_digit(pch) {
            return true;
        }
        if is_digit(c) {
            return true;
        }
        false
    };

    let mut hits: Vec<usize> = Vec::with_capacity(p_len);
    let mut ti = 0usize;
    for pi in 0..p_len {
        let pc = lower(p[pi]);
        let mut found = false;
        while ti < t_len {
            if matchable(ti) && lower(t[ti]) == pc {
                hits.push(ti);
                ti += 1;
                found = true;
                break;
            }
            ti += 1;
        }
        if !found {
            return 0;
        }
    }

    // first hit at index 0 → ~600; later word-start → ~400; plus tightness.
    let mut score = if hits[0] == 0 { 600 } else { 400 };
    let span = hits[hits.len() - 1] - hits[0] + 1;
    score += (50 - (span as i32 - p_len as i32)).max(0);
    let score = score.max(1);
    if let Some(out) = out_positions {
        *out = hits;
    }
    score
}

/// `CommandPalette::fuzzyScore` (`commandpalette.h:132`) — the linear single-pass
/// scorer over "Menu > Sub > Item" paths.
///
/// Walks the haystack once; per matched char: base 1, +2 contiguous run, +3 if
/// the previous haystack char was a separator (` >/_-`), +1 leading-prefix
/// (`hi == ni`). Returns `0` if not all of `needle` is consumed, `1` for an empty
/// needle. (The palette does not paint per-char highlights, so there is no
/// position output — matching the C++ signature.)
pub fn command_score(needle: &str, haystack: &str) -> i32 {
    let n: Vec<char> = needle.chars().collect();
    let h: Vec<char> = haystack.chars().collect();
    if n.is_empty() {
        return 1;
    }
    let mut ni = 0usize;
    let mut hi = 0usize;
    let mut score = 0i32;
    let mut run = 0i32;
    let mut prev_sep = true; // start of string counts as separator
    while ni < n.len() && hi < h.len() {
        let nc = lower(n[ni]);
        let hc = lower(h[hi]);
        if nc == hc {
            let mut bonus = 1;
            if run > 0 {
                bonus += 2; // contiguous match
            }
            if prev_sep {
                bonus += 3; // word-start
            }
            if hi == ni {
                bonus += 1; // leading-prefix bias
            }
            score += bonus;
            run += 1;
            ni += 1;
        } else {
            run = 0;
        }
        let raw = h[hi];
        prev_sep = raw == ' ' || raw == '>' || raw == '/' || raw == '_' || raw == '-';
        hi += 1;
    }
    if ni < n.len() {
        return 0; // not all needle matched
    }
    score
}

/// `SourceChooserPopup::fuzzyScore` (`sourcechooserpopup.cpp:25`) — recursive
/// backtracking matcher (branch cap 4) for the source-chooser cards.
///
/// Beyond the length caps (`pattern > 64` or `text > 256`) it degrades to a
/// case-insensitive prefix check (score `1`/`0`). Otherwise it does a fast
/// subsequence reject, then a bounded DFS keeping the best-scoring positions:
/// per char base 1, 10 at index 0, 8 after `_`/space or at a CamelCase boundary,
/// +5 contiguous; plus a final tightness (`max(0, 20 - gap)`) and +20 exact
/// length. `out_positions`, when `Some`, receives the best match's char indices.
pub fn source_score(pattern: &str, text: &str, out_positions: Option<&mut Vec<usize>>) -> i32 {
    let pattern = SourceScorePattern::new(pattern);
    let text = SourceScoreText::new(text);
    source_score_prepared(&pattern, &text, out_positions)
}

pub fn source_score_prepared(
    pattern: &SourceScorePattern,
    text: &SourceScoreText,
    out_positions: Option<&mut Vec<usize>>,
) -> i32 {
    let p_len = pattern.chars.len();
    let t_len = text.chars.len();

    if p_len == 0 {
        return 1;
    }
    if p_len > t_len {
        return 0;
    }
    if p_len > MAX_FUZZY_LEN || t_len > 256 {
        // Degrade to a prefix check beyond the caps.
        if index_of_ci(&text.chars, &pattern.chars) == Some(0) {
            return 1;
        }
        return 0;
    }

    // Fast subsequence reject (the C++ pre-pass).
    {
        let mut pi = 0usize;
        let mut ti = 0usize;
        while ti < t_len && pi < p_len {
            if pattern.lower[pi] == text.lower[ti] {
                pi += 1;
            }
            ti += 1;
        }
        if pi < p_len {
            return 0;
        }
    }

    let mut best = 0i32;
    let mut best_pos: Vec<usize> = Vec::new();
    let mut cur_pos: Vec<usize> = vec![0usize; p_len];

    // Recursive DFS, branch cap 4 per level (matches the C++ lambda).
    fn solve(
        pi: usize,
        ti: usize,
        cur_len: usize,
        score: i32,
        p_len: usize,
        t_len: usize,
        p_low: &[char],
        t_low: &[char],
        t: &[char],
        cur_pos: &mut Vec<usize>,
        best: &mut i32,
        best_pos: &mut Vec<usize>,
    ) {
        if pi == p_len {
            if score > *best {
                *best = score;
                *best_pos = cur_pos[..cur_len].to_vec();
            }
            return;
        }
        let max_ti = t_len - (p_len - pi);
        let mut branches = 0;
        let mut i = ti;
        while i <= max_ti && branches < 4 {
            if p_low[pi] != t_low[i] {
                i += 1;
                continue;
            }
            let mut bonus = 1;
            if i == 0 {
                bonus = 10;
            } else if t[i - 1] == '_' || t[i - 1] == ' ' {
                bonus = 8;
            } else if is_upper(t[i]) && is_lower(t[i - 1]) {
                bonus = 8;
            }
            if cur_len > 0 && i == cur_pos[cur_len - 1] + 1 {
                bonus += 5;
            }
            cur_pos[cur_len] = i;
            solve(
                pi + 1,
                i + 1,
                cur_len + 1,
                score + bonus,
                p_len,
                t_len,
                p_low,
                t_low,
                t,
                cur_pos,
                best,
                best_pos,
            );
            branches += 1;
            i += 1;
        }
    }

    solve(
        0,
        0,
        0,
        0,
        p_len,
        t_len,
        &pattern.lower,
        &text.lower,
        &text.chars,
        &mut cur_pos,
        &mut best,
        &mut best_pos,
    );

    if best > 0 {
        best += (20 - (t_len as i32 - p_len as i32)).max(0);
        if p_len == t_len {
            best += 20;
        }
        if let Some(out) = out_positions {
            *out = best_pos;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::{
        command_score, fuzzy_score, source_score, source_score_prepared, SourceScorePattern,
        SourceScoreText,
    };

    // ── command_score (CommandPalette::fuzzyScore) — test_command_palette.cpp ──

    #[test]
    fn command_empty_needle_matches_all() {
        // Empty needle → score 1 (everything matches).
        assert_eq!(command_score("", "File > Save"), 1);
        assert_eq!(command_score("", ""), 1);
    }

    #[test]
    fn command_word_start_beats_mid_word() {
        // "Open" as a word-start ("File > Open") outscores the mid-word hit in
        // "File > Reopen".
        let word_start = command_score("Open", "File > Open");
        let mid_word = command_score("Open", "File > Reopen");
        assert!(word_start > 0);
        assert!(mid_word > 0);
        assert!(
            word_start > mid_word,
            "word-start {word_start} should beat mid-word {mid_word}"
        );
    }

    #[test]
    fn command_miss_returns_zero() {
        assert_eq!(command_score("zzz", "File > Save"), 0);
        // Not all needle consumed → 0.
        assert_eq!(command_score("Savex", "File > Save"), 0);
    }

    #[test]
    fn command_acronym_matches() {
        // "fs" → "File > Save" matches (F at start, S after separator) > 0.
        assert!(command_score("fs", "File > Save") > 0);
    }

    #[test]
    fn command_case_insensitive_equal() {
        assert_eq!(
            command_score("save", "File > Save"),
            command_score("SAVE", "File > Save")
        );
        assert_eq!(
            command_score("FiLe", "File > Save"),
            command_score("file", "File > Save")
        );
    }

    #[test]
    fn command_leading_prefix_bonus() {
        // "Fi" at the very start gets the leading-prefix bias on each char.
        assert!(command_score("Fi", "File > Save") > 0);
    }

    // ── fuzzy_score (rcx::fuzzyScore) — test_type_selector.cpp / fuzzy_match.h ──

    #[test]
    fn fuzzy_empty_pattern_is_one() {
        assert_eq!(fuzzy_score("", "anything", None), 1);
    }

    #[test]
    fn fuzzy_pattern_longer_than_text_is_zero() {
        assert_eq!(fuzzy_score("toolong", "ab", None), 0);
    }

    #[test]
    fn fuzzy_prefix_substring_outscores_internal() {
        // Prefix (idx 0) gets +500; internal substring gets base only.
        let prefix = fuzzy_score("Get", "GetProcAddress", None);
        let internal = fuzzy_score("Proc", "GetProcAddress", None);
        assert!(prefix > 0 && internal > 0);
        assert!(
            prefix > internal,
            "prefix {prefix} should beat internal {internal}"
        );
    }

    #[test]
    fn fuzzy_exact_match_is_highest() {
        // Exact, full-length, prefix match: base 1000 + 500 + tightness(100) +
        // exact-length(200).
        let exact = fuzzy_score("int32_t", "int32_t", None);
        let prefix_only = fuzzy_score("int", "int32_t", None);
        assert!(exact > prefix_only);
    }

    #[test]
    fn fuzzy_substring_positions_are_contiguous() {
        let mut pos = Vec::new();
        let s = fuzzy_score("Proc", "GetProcAddress", Some(&mut pos));
        assert!(s > 0);
        // "Proc" begins at index 3 in "GetProcAddress".
        assert_eq!(pos, vec![3, 4, 5, 6]);
    }

    #[test]
    fn fuzzy_acronym_word_start_initials() {
        // "GPA" → GetProcAddress (G at 0, P at CamelCase boundary, A at boundary).
        let mut pos = Vec::new();
        let s = fuzzy_score("GPA", "GetProcAddress", Some(&mut pos));
        assert!(s > 0, "GPA should match GetProcAddress via initials");
        assert_eq!(pos, vec![0, 3, 7]);
    }

    #[test]
    fn fuzzy_digit_initials_match_uint() {
        // "u32" → uint32_t (u at 0; '3' is a digit / letter→digit; '2' a digit).
        let mut pos = Vec::new();
        let s = fuzzy_score("u32", "uint32_t", Some(&mut pos));
        assert!(s > 0, "u32 should match uint32_t");
        assert_eq!(pos, vec![0, 4, 5]);
    }

    #[test]
    fn fuzzy_scattered_subsequence_rejected() {
        // The whole point of the two-pass matcher: "Test" must NOT match
        // "TerminateSomething" as a scattered T·e·s·t subsequence.
        assert_eq!(fuzzy_score("Test", "TerminateSomething", None), 0);
    }

    #[test]
    fn fuzzy_case_insensitive() {
        assert_eq!(
            fuzzy_score("get", "GetProcAddress", None),
            fuzzy_score("GET", "GetProcAddress", None)
        );
    }

    // ── source_score (SourceChooserPopup::fuzzyScore) ──

    #[test]
    fn source_empty_pattern_is_one() {
        assert_eq!(source_score("", "notepad.exe", None), 1);
    }

    #[test]
    fn source_prefix_scores_high_with_positions() {
        let mut pos = Vec::new();
        let s = source_score("note", "notepad.exe", Some(&mut pos));
        assert!(s > 0);
        // Contiguous prefix match at 0..4.
        assert_eq!(pos, vec![0, 1, 2, 3]);
    }

    #[test]
    fn source_miss_returns_zero() {
        assert_eq!(source_score("zzz", "notepad.exe", None), 0);
    }

    #[test]
    fn source_word_start_after_separator_beats_internal() {
        // After a space the bonus is 8; an internal char is base 1.
        let after_sep = source_score("c", "notepad calc", None);
        let internal = source_score("d", "notepad", None);
        assert!(after_sep > 0 && internal > 0);
        assert!(after_sep > internal);
    }

    #[test]
    fn source_exact_length_bonus() {
        let exact = source_score("calc", "calc", None);
        let longer = source_score("calc", "calculator", None);
        assert!(exact > longer, "exact-length match should score higher");
    }

    #[test]
    fn source_case_insensitive() {
        assert_eq!(
            source_score("note", "Notepad", None),
            source_score("NOTE", "Notepad", None)
        );
    }

    #[test]
    fn source_prepared_matches_public_wrapper() {
        let pattern = SourceScorePattern::new("pc");
        let text = SourceScoreText::new("PlayerComponent_0042");
        let mut direct_pos = Vec::new();
        let mut prepared_pos = Vec::new();
        let direct = source_score("pc", "PlayerComponent_0042", Some(&mut direct_pos));
        let prepared = source_score_prepared(&pattern, &text, Some(&mut prepared_pos));
        assert_eq!(prepared, direct);
        assert_eq!(prepared_pos, direct_pos);
    }
}
