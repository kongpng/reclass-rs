//! Find bar (Ctrl+F) — incremental search over the rendered editor lines
//! (`editor.cpp` `showFindBar`/`doFind`, editor-surface.md; Edit ▸ Find Field).
//!
//! Port of the editor's find bar: a small input that, as the user types,
//! highlights **every** match in the document and advances a wrapping cursor to
//! the next/previous match (`doFind(forward)` paints all hits then navigates from
//! `m_findPos`). The C++ drives a Scintilla indicator + cursor; here the search is
//! modeled purely over the composed line texts (the GPUI editor renders lines
//! from [`crate::compose`], not a Scintilla buffer), so the match set + the
//! next/prev wrap logic are gpui-free and unit-tested. The view paints the
//! highlights and scrolls to the current match.
//!
//! Search is **case-insensitive substring** (matching the C++ default search
//! flags = 0, which is case-insensitive in this build), with overlapping matches
//! advanced by `max(needle_len, 1)` exactly as `doFind` scans.
//!
//! Gated behind the `ui` feature.

/// One match: the line index + the `[start, end)` char range within that line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FindMatch {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// The find-bar search state — the gpui-free core (`m_findBar` text + `m_findPos`
/// cursor + the painted match set).
///
/// Feed it the rendered line texts + the query; it computes all matches (for the
/// global highlight) and tracks a current match for next/prev navigation with
/// wraparound. Empty query → no matches, no current.
#[derive(Clone, Debug, Default)]
pub struct FindState {
    query: String,
    matches: Vec<FindMatch>,
    /// Index into `matches` of the current (focused) match, if any.
    current: Option<usize>,
}

impl FindState {
    /// A fresh, empty find state (the bar is hidden / query empty).
    pub fn new() -> Self {
        FindState::default()
    }

    /// The current query text.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// All matches in document order (for painting every hit; `doFind` pass 1).
    pub fn matches(&self) -> &[FindMatch] {
        &self.matches
    }

    /// The number of matches.
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// The current (focused) match, if any.
    pub fn current_match(&self) -> Option<FindMatch> {
        self.current.and_then(|i| self.matches.get(i).copied())
    }

    /// The 1-based position of the current match (for an "N of M" readout), or 0.
    pub fn current_ordinal(&self) -> usize {
        self.current.map(|i| i + 1).unwrap_or(0)
    }

    /// Set the query and recompute the match set over `lines`
    /// (`textChanged` → `m_findPos = 0; doFind(true)`): all case-insensitive
    /// substring hits across the lines, then select the first match (cursor reset
    /// to the document start, advance forward).
    pub fn set_query(&mut self, query: &str, lines: &[String]) {
        self.query = query.to_string();
        self.recompute(lines);
        // Cursor reset to start; the first forward match becomes current.
        self.current = if self.matches.is_empty() {
            None
        } else {
            Some(0)
        };
    }

    /// Recompute the match set for the current query over `lines` (without moving
    /// the cursor) — e.g. after the document changed while the bar is open.
    pub fn recompute(&mut self, lines: &[String]) {
        self.matches.clear();
        if self.query.is_empty() {
            self.current = None;
            return;
        }
        if self.query.is_ascii() {
            self.recompute_ascii(lines);
        } else {
            self.recompute_unicode(lines);
        }
        // Keep the current index in range if still valid.
        if let Some(c) = self.current {
            if c >= self.matches.len() {
                self.current = if self.matches.is_empty() {
                    None
                } else {
                    Some(0)
                };
            }
        }
    }

    fn recompute_ascii(&mut self, lines: &[String]) {
        let needle = self.query.as_bytes().to_vec();
        let step = needle.len().max(1);
        for (li, line) in lines.iter().enumerate() {
            if !line.is_ascii() {
                self.recompute_unicode_line(li, line);
                continue;
            }
            let hay = line.as_bytes();
            if needle.len() > hay.len() {
                continue;
            }
            let mut i = 0usize;
            while i + needle.len() <= hay.len() {
                if hay[i..i + needle.len()].eq_ignore_ascii_case(&needle) {
                    self.matches.push(FindMatch {
                        line: li,
                        start: i,
                        end: i + needle.len(),
                    });
                    i += step;
                } else {
                    i += 1;
                }
            }
        }
    }

    fn recompute_unicode(&mut self, lines: &[String]) {
        let needle = self.query.to_lowercase();
        let needle_chars: Vec<char> = needle.chars().collect();
        for (li, line) in lines.iter().enumerate() {
            self.recompute_unicode_chars(li, line, &needle_chars);
        }
    }

    fn recompute_unicode_line(&mut self, line_idx: usize, line: &str) {
        let needle = self.query.to_lowercase();
        let needle_chars: Vec<char> = needle.chars().collect();
        self.recompute_unicode_chars(line_idx, line, &needle_chars);
    }

    fn recompute_unicode_chars(&mut self, line_idx: usize, line: &str, needle_chars: &[char]) {
        if needle_chars.is_empty() {
            self.current = None;
            return;
        }
        let step = needle_chars.len().max(1);
        let hay: Vec<char> = line.to_lowercase().chars().collect();
        if needle_chars.len() > hay.len() {
            return;
        }
        let mut i = 0usize;
        while i + needle_chars.len() <= hay.len() {
            if hay[i..i + needle_chars.len()] == needle_chars[..] {
                self.matches.push(FindMatch {
                    line: line_idx,
                    start: i,
                    end: i + needle_chars.len(),
                });
                i += step;
            } else {
                i += 1;
            }
        }
    }

    /// Advance to the next match with wraparound (`doFind(true)` →
    /// findNext). No-op if there are no matches.
    pub fn next(&mut self) -> Option<FindMatch> {
        if self.matches.is_empty() {
            self.current = None;
            return None;
        }
        let next = match self.current {
            Some(c) => (c + 1) % self.matches.len(),
            None => 0,
        };
        self.current = Some(next);
        self.current_match()
    }

    /// Step to the previous match with wraparound (`doFind(false)` →
    /// findPrev). No-op if there are no matches.
    pub fn prev(&mut self) -> Option<FindMatch> {
        if self.matches.is_empty() {
            self.current = None;
            return None;
        }
        let prev = match self.current {
            Some(0) | None => self.matches.len() - 1,
            Some(c) => c - 1,
        };
        self.current = Some(prev);
        self.current_match()
    }

    /// Clear the search (`hideFindBar` clears the indicator + query).
    pub fn clear(&mut self) {
        self.query.clear();
        self.matches.clear();
        self.current = None;
    }
}

// ── gpui view ───────────────────────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use view::{find_bar_key_bindings, FindBar, FindEvent};

#[cfg(feature = "ui")]
mod view {
    use super::{FindMatch, FindState};
    use crate::ui::design::{color, tokens};
    use gpui::*;
    use gpui_component::input::{Input, InputEvent, InputState};

    actions!(rcx_find_bar, [FindNext, FindPrev, FindClose]);

    /// The find-bar key bindings (bound in the `RcxFindBar` context).
    pub fn find_bar_key_bindings() -> Vec<KeyBinding> {
        vec![
            KeyBinding::new("enter", FindNext, Some("RcxFindBar")),
            KeyBinding::new("shift-enter", FindPrev, Some("RcxFindBar")),
            KeyBinding::new("escape", FindClose, Some("RcxFindBar")),
        ]
    }

    /// What the find bar reports to its host (the editor): navigate to a match, or
    /// close the bar.
    #[derive(Clone, Debug)]
    pub enum FindEvent {
        /// Move the editor cursor/scroll to this match.
        Navigate(FindMatch),
        /// Close the find bar (Esc / close button).
        Close,
    }

    /// The find-bar overlay view. The host supplies the current line texts (the
    /// rendered editor lines) via [`set_lines`](FindBar::set_lines) whenever the
    /// document changes.
    pub struct FindBar {
        state: FindState,
        lines: Vec<String>,
        input: Entity<InputState>,
        focus_handle: FocusHandle,
        _subscription: Subscription,
    }

    impl FindBar {
        /// Build the find bar over the given initial line texts.
        pub fn new(lines: Vec<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find..."));
            let subscription =
                cx.subscribe_in(&input, window, |this, _i, ev: &InputEvent, _window, cx| {
                    if matches!(ev, InputEvent::Change) {
                        let q = this.input.read(cx).value().to_string();
                        let lines = this.lines.clone();
                        this.state.set_query(&q, &lines);
                        if let Some(m) = this.state.current_match() {
                            cx.emit(FindEvent::Navigate(m));
                        }
                        cx.notify();
                    }
                });
            FindBar {
                state: FindState::new(),
                lines,
                input,
                focus_handle: cx.focus_handle(),
                _subscription: subscription,
            }
        }

        /// Read-only access to the search state (tests / wiring).
        pub fn state(&self) -> &FindState {
            &self.state
        }

        /// Update the searched line texts (the document changed); re-runs the
        /// search over the new lines.
        pub fn set_lines(&mut self, lines: Vec<String>, cx: &mut Context<Self>) {
            self.lines = lines;
            self.state.recompute(&self.lines);
            cx.notify();
        }

        fn on_next(&mut self, _: &FindNext, _: &mut Window, cx: &mut Context<Self>) {
            if let Some(m) = self.state.next() {
                cx.emit(FindEvent::Navigate(m));
            }
            cx.notify();
        }
        fn on_prev(&mut self, _: &FindPrev, _: &mut Window, cx: &mut Context<Self>) {
            if let Some(m) = self.state.prev() {
                cx.emit(FindEvent::Navigate(m));
            }
            cx.notify();
        }
        fn on_close(&mut self, _: &FindClose, _: &mut Window, cx: &mut Context<Self>) {
            // Item 33: the C++ `hideFindBar` PRESERVES `m_findPos` and the IND_FIND
            // highlights — it does NOT clear the search state. Just emit Close; the
            // host hides the bar and remembers the query so re-showing resumes.
            cx.emit(FindEvent::Close);
        }

        /// The current query text (so the host can persist it across hide/show,
        /// item 33).
        pub fn query(&self) -> &str {
            self.state.query()
        }

        /// Re-seed the query (resuming a prior search on re-show) and re-run it
        /// over the current lines. Item 33.
        pub fn set_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
            self.input
                .update(cx, |i, cx| i.set_value(query, window, cx));
            let lines = self.lines.clone();
            self.state.set_query(query, &lines);
            cx.notify();
        }
    }

    impl Focusable for FindBar {
        /// Return the FILTER INPUT's focus handle so the editor's
        /// `window.focus(bar.focus_handle)` on Ctrl+F lands keystrokes in the
        /// field — opening the find bar and immediately typing searches (the C++
        /// find bar focuses its line-edit on show). Without this the bar's outer
        /// `track_focus` div holds focus and typing does nothing (the dead-input
        /// failure mode the cross-cutting note flags). The `RcxFindBar`
        /// `key_context` + `on_action` handlers still fire for Enter/Shift+Enter/
        /// Escape because the focused input is a descendant of the `track_focus`
        /// surface.
        fn focus_handle(&self, cx: &App) -> FocusHandle {
            self.input.read(cx).focus_handle(cx)
        }
    }

    impl EventEmitter<FindEvent> for FindBar {}

    impl Render for FindBar {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui_component::button::{Button, ButtonVariants as _};
            use gpui_component::{IconName, Sizable as _};
            let count = self.state.match_count();
            let ordinal = self.state.current_ordinal();
            let no_results = !self.state.query().is_empty() && count == 0;
            let readout = if self.state.query().is_empty() {
                String::new()
            } else if count == 0 {
                "No results".to_string()
            } else {
                format!("{ordinal} of {count}")
            };
            let readout_color = if no_results {
                color::danger(cx)
            } else {
                color::text_muted(cx)
            };

            // A compact Zed inline search bar: an elevated rounded pill holding the
            // input, the match-count readout, prev/next nav, and a close button.
            gpui_component::h_flex()
                .id("rcx-find-bar")
                .track_focus(&self.focus_handle)
                .key_context("RcxFindBar")
                .on_action(cx.listener(Self::on_next))
                .on_action(cx.listener(Self::on_prev))
                .on_action(cx.listener(Self::on_close))
                .gap(px(tokens::space::XS))
                .px(px(tokens::space::MD))
                .py(px(tokens::space::SM))
                .items_center()
                .bg(color::elevated_bg(cx))
                .border_b_1()
                .border_color(color::border(cx))
                .shadow_md()
                .child(Input::new(&self.input).small().w(px(240.)))
                .child(
                    div()
                        .min_w(px(64.))
                        .text_size(px(tokens::font::UI_SM))
                        .text_color(readout_color)
                        .child(readout),
                )
                .child(
                    Button::new("find-prev")
                        .ghost()
                        .small()
                        .icon(IconName::ChevronUp)
                        .on_click(
                            cx.listener(|this, _e, window, cx| this.on_prev(&FindPrev, window, cx)),
                        ),
                )
                .child(
                    Button::new("find-next")
                        .ghost()
                        .small()
                        .icon(IconName::ChevronDown)
                        .on_click(
                            cx.listener(|this, _e, window, cx| this.on_next(&FindNext, window, cx)),
                        ),
                )
                .child(
                    Button::new("find-close")
                        .ghost()
                        .small()
                        .icon(IconName::Close)
                        .on_click(cx.listener(|this, _e, window, cx| {
                            this.on_close(&FindClose, window, cx)
                        })),
                )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FindMatch, FindState};

    fn lines() -> Vec<String> {
        vec![
            "struct Player".to_string(),
            "  float health".to_string(),
            "  float stamina".to_string(),
            "  int32_t score".to_string(),
        ]
    }

    #[test]
    fn empty_query_has_no_matches() {
        let mut s = FindState::new();
        s.set_query("", &lines());
        assert_eq!(s.match_count(), 0);
        assert_eq!(s.current_match(), None);
        assert_eq!(s.current_ordinal(), 0);
    }

    #[test]
    fn finds_all_matches_case_insensitively() {
        let mut s = FindState::new();
        s.set_query("FLOAT", &lines());
        // "float" appears on lines 1 and 2.
        assert_eq!(s.match_count(), 2);
        let m = s.matches();
        assert_eq!(
            m[0],
            FindMatch {
                line: 1,
                start: 2,
                end: 7
            }
        );
        assert_eq!(
            m[1],
            FindMatch {
                line: 2,
                start: 2,
                end: 7
            }
        );
        // First match is current.
        assert_eq!(s.current_match(), Some(m[0]));
        assert_eq!(s.current_ordinal(), 1);
    }

    #[test]
    fn ascii_query_still_searches_non_ascii_lines() {
        let mut s = FindState::new();
        s.set_query("CAF", &["  Café value".to_string()]);
        assert_eq!(
            s.matches(),
            &[FindMatch {
                line: 0,
                start: 2,
                end: 5
            }]
        );
    }

    #[test]
    fn next_wraps_around() {
        let mut s = FindState::new();
        s.set_query("float", &lines());
        assert_eq!(s.current_ordinal(), 1);
        let m2 = s.next().unwrap();
        assert_eq!(m2.line, 2);
        assert_eq!(s.current_ordinal(), 2);
        // Wrap back to the first.
        let m1 = s.next().unwrap();
        assert_eq!(m1.line, 1);
        assert_eq!(s.current_ordinal(), 1);
    }

    #[test]
    fn prev_wraps_around() {
        let mut s = FindState::new();
        s.set_query("float", &lines());
        // From the first match, prev wraps to the last.
        let last = s.prev().unwrap();
        assert_eq!(last.line, 2);
        assert_eq!(s.current_ordinal(), 2);
    }

    #[test]
    fn single_match_navigation_is_stable() {
        let mut s = FindState::new();
        s.set_query("Player", &lines());
        assert_eq!(s.match_count(), 1);
        // next/prev on a single match keep it current.
        assert_eq!(s.next().unwrap().line, 0);
        assert_eq!(s.prev().unwrap().line, 0);
        assert_eq!(s.current_ordinal(), 1);
    }

    #[test]
    fn no_match_query() {
        let mut s = FindState::new();
        s.set_query("zzz", &lines());
        assert_eq!(s.match_count(), 0);
        assert!(s.next().is_none());
        assert!(s.prev().is_none());
    }

    #[test]
    fn overlapping_matches_advance_by_needle_len() {
        // "aa" in "aaaa" → 2 non-overlapping matches at 0 and 2 (step = len).
        let mut s = FindState::new();
        s.set_query("aa", &["aaaa".to_string()]);
        assert_eq!(s.match_count(), 2);
        assert_eq!(
            s.matches()[0],
            FindMatch {
                line: 0,
                start: 0,
                end: 2
            }
        );
        assert_eq!(
            s.matches()[1],
            FindMatch {
                line: 0,
                start: 2,
                end: 4
            }
        );
    }

    #[test]
    fn recompute_keeps_or_resets_current() {
        let mut s = FindState::new();
        s.set_query("float", &lines());
        s.next(); // current = 1
        assert_eq!(s.current_ordinal(), 2);
        // Document shrinks to one line with one "float" → current clamps to 0.
        s.recompute(&["  float x".to_string()]);
        assert_eq!(s.match_count(), 1);
        assert_eq!(s.current_ordinal(), 1);
    }

    #[test]
    fn clear_resets_everything() {
        let mut s = FindState::new();
        s.set_query("float", &lines());
        s.clear();
        assert_eq!(s.query(), "");
        assert_eq!(s.match_count(), 0);
        assert_eq!(s.current_match(), None);
    }
}
