//! Options dialog — the settings nav-tree + stacked-pages + search dialog
//! (`optionsdialog.{h,cpp}`, widgets-dialogs.md §5).
//!
//! Port of `OptionsDialog`: a fixed 700×450 dialog with a left nav tree
//! (Environment → General / AI Features / Generator) + a search box that filters
//! the tree, and a right page stack reading/writing an [`OptionsResult`]. Per the
//! cookbook (ARCHITECTURE §5) it maps onto a gpui-component `Dialog` + `Tree` +
//! conditional page render + a `TextInput` filter.
//!
//! Split (gpui-free model + thin view, like the prior stages):
//! - [`OptionsResult`] — the C++ `struct OptionsResult` round-tripped by the
//!   dialog (`themeIndex`/`fontName`/…); the controls read/write it.
//! - [`OptionsPage`] — the three pages (General/AI/Generator) + their nav labels,
//!   keywords (for search), and the controls each hosts.
//! - [`filter_visible`] — the recursive tree search filter (`filterTree`),
//!   unit-tested against the C++ rule (name OR page-keywords OR any child).
//! - [`FONT_CHOICES`] — the three font combo items (source-faithful, not the
//!   stale 2-item test; widgets-dialogs §5 / §24 Q1).
//! - [`OptionsDialog`] / [`OptionsEvent`] — the gpui view raising `Apply(result)`
//!   / `Cancel`.
//!
//! Gated behind the `ui` feature.

/// `struct OptionsResult` (`optionsdialog.h:13`) — the persisted options the
/// dialog reads on open and writes back on OK.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OptionsResult {
    /// Selected theme index (`m_themeCombo`).
    pub theme_index: usize,
    /// Editor/UI font name (`m_fontCombo`).
    pub font_name: String,
    /// Uppercase menu items (`m_titleCaseCheck`).
    pub menu_bar_title_case: bool,
    /// Show icon in the title bar (`m_showIconCheck`).
    pub show_icon: bool,
    /// Auto-start the MCP server (`m_autoMcpCheck`).
    pub auto_start_mcp: bool,
    /// Refresh interval in ms (`m_refreshSpin`, range 1..=60000).
    pub refresh_ms: i32,
    /// Emit `static_assert` size checks in generated C++ (`m_assertCheck`).
    pub generator_asserts: bool,
    /// Opening brace on a new line (`m_braceWrapCheck`).
    pub brace_wrap: bool,
}

impl Default for OptionsResult {
    /// The C++ default-member values (`optionsdialog.h:13-21`).
    fn default() -> Self {
        OptionsResult {
            theme_index: 0,
            font_name: String::new(),
            menu_bar_title_case: true,
            show_icon: false,
            auto_start_mcp: true,
            refresh_ms: 660,
            generator_asserts: false,
            brace_wrap: false,
        }
    }
}

impl OptionsResult {
    /// Clamp the refresh interval to the spin range (1..=60000) — the C++ spin
    /// clamps `0 → 1` (`test_options_dialog.cpp:263`).
    pub fn clamp_refresh(&mut self) {
        self.refresh_ms = self.refresh_ms.clamp(REFRESH_MIN, REFRESH_MAX);
    }
}

/// The refresh-spin range (`m_refreshSpin`, `optionsdialog.cpp`: range 1..60000).
pub const REFRESH_MIN: i32 = 1;
pub const REFRESH_MAX: i32 = 60000;
/// The default refresh description value (660 ms).
pub const REFRESH_DEFAULT: i32 = 660;

/// The font combo items (`optionsdialog.cpp:111-113`) — three, per the **source**
/// (the lagging test asserts two; widgets-dialogs §24 Q1 resolves to the source).
pub const FONT_CHOICES: [&str; 3] = ["IBM Plex Mono", "JetBrains Mono", "Consolas"];

/// The three settings pages (`m_pages`, `optionsdialog.cpp`). The integer value
/// is the page index the nav tree maps to (`m_itemPageIndex`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OptionsPage {
    /// Page 0 — General (refresh rate + visual experience).
    General,
    /// Page 1 — AI Features (MCP server).
    AiFeatures,
    /// Page 2 — Generator (C++ header options).
    Generator,
}

impl OptionsPage {
    /// The pages in nav order.
    pub const ALL: [OptionsPage; 3] = [
        OptionsPage::General,
        OptionsPage::AiFeatures,
        OptionsPage::Generator,
    ];

    /// The page index (`m_itemPageIndex` value).
    pub fn index(self) -> usize {
        match self {
            OptionsPage::General => 0,
            OptionsPage::AiFeatures => 1,
            OptionsPage::Generator => 2,
        }
    }

    /// The nav-tree label (`QTreeWidgetItem` text).
    pub fn nav_label(self) -> &'static str {
        match self {
            OptionsPage::General => "General",
            OptionsPage::AiFeatures => "AI Features",
            OptionsPage::Generator => "Generator",
        }
    }

    /// The page's searchable keywords — every label/checkbox/groupbox-title/combo
    /// item under the page (`collectPageKeywords`, `optionsdialog.cpp:235`). Drives
    /// the tree search filter so e.g. "MCP" surfaces the AI Features page.
    pub fn keywords(self) -> Vec<&'static str> {
        match self {
            OptionsPage::General => {
                let mut kw = vec![
                    "Refresh Rate",
                    "Visual Experience",
                    "Uppercase menu items",
                    "Show icon in title bar",
                    "Opening brace on new line",
                ];
                kw.extend(FONT_CHOICES);
                kw
            }
            OptionsPage::AiFeatures => vec!["MCP Server", "Auto-start MCP server"],
            OptionsPage::Generator => vec!["C++ Header", "Emit static_assert size checks"],
        }
    }
}

/// The recursive tree search filter (`filterTree`, `optionsdialog.cpp:252`).
///
/// An item is visible if its label contains `query` (case-insensitive) **OR** any
/// of its [`keywords`](OptionsPage::keywords) contains it **OR** any child is
/// visible. Here the tree is the flat Environment → {General, AI, Generator}, so
/// "a page is visible" = its own label/keywords match (the parent "Environment"
/// is then visible because a child is). Returns the visible pages in nav order.
pub fn filter_visible(query: &str) -> Vec<OptionsPage> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return OptionsPage::ALL.to_vec();
    }
    OptionsPage::ALL
        .iter()
        .copied()
        .filter(|p| page_matches(*p, &q))
        .collect()
}

/// Whether a page matches the (already-lowercased, trimmed) query — label OR any
/// keyword contains it.
fn page_matches(page: OptionsPage, q_lower: &str) -> bool {
    if page.nav_label().to_lowercase().contains(q_lower) {
        return true;
    }
    page.keywords()
        .iter()
        .any(|kw| kw.to_lowercase().contains(q_lower))
}

// ── gpui view ───────────────────────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use view::{OptionsDialog, OptionsEvent};

#[cfg(feature = "ui")]
mod view {
    use super::{filter_visible, OptionsPage, OptionsResult, FONT_CHOICES, REFRESH_DEFAULT};
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::input::{Input, InputEvent, InputState};
    use gpui_component::{ActiveTheme, Selectable as _};

    /// The dialog's outcome (the C++ `accept`/`reject`).
    #[derive(Clone, Debug)]
    pub enum OptionsEvent {
        /// OK pressed — apply the (read-back) result.
        Apply(OptionsResult),
        /// Cancel / Esc.
        Cancel,
    }

    /// The Options dialog view: a search box + nav list (left) and the selected
    /// page's controls (right), reading/writing a live [`OptionsResult`].
    pub struct OptionsDialog {
        result: OptionsResult,
        /// Available theme names (filled from the manager; the combo's items).
        themes: Vec<String>,
        page: OptionsPage,
        search: Entity<InputState>,
        query: String,
        focus_handle: FocusHandle,
        _subscription: Subscription,
    }

    impl OptionsDialog {
        /// Build the dialog seeded from `current` and the available theme names.
        pub fn new(
            current: OptionsResult,
            themes: Vec<String>,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> Self {
            let search =
                cx.new(|cx| InputState::new(window, cx).placeholder("Search Options (Ctrl+E)"));
            let subscription =
                cx.subscribe_in(&search, window, |this, _s, ev: &InputEvent, _window, cx| {
                    if matches!(ev, InputEvent::Change) {
                        this.query = this.search.read(cx).value().to_string();
                        // If the selected page is filtered out, jump to the first
                        // visible page (the C++ tree hides non-matching items).
                        let visible = filter_visible(&this.query);
                        if !visible.is_empty() && !visible.contains(&this.page) {
                            this.page = visible[0];
                        }
                        cx.notify();
                    }
                });
            OptionsDialog {
                result: current,
                themes,
                page: OptionsPage::General,
                search,
                query: String::new(),
                focus_handle: cx.focus_handle(),
                _subscription: subscription,
            }
        }

        /// Read-only access to the live result (for tests / wiring).
        pub fn result(&self) -> &OptionsResult {
            &self.result
        }

        /// The currently selected page.
        pub fn page(&self) -> OptionsPage {
            self.page
        }

        /// Select a page (the C++ `selectPage` / `currentItemChanged`).
        pub fn select_page(&mut self, page: OptionsPage, cx: &mut Context<Self>) {
            self.page = page;
            cx.notify();
        }

        fn confirm(&mut self, cx: &mut Context<Self>) {
            let mut r = self.result.clone();
            r.clamp_refresh();
            cx.emit(OptionsEvent::Apply(r));
        }

        fn cancel(&mut self, cx: &mut Context<Self>) {
            cx.emit(OptionsEvent::Cancel);
        }

        /// Render the left nav: the filtered page list.
        fn render_nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui_component::button::{Button, ButtonVariants as _};
            let visible = filter_visible(&self.query);
            let mut col = gpui_component::v_flex().w(px(200.)).gap_0p5().child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Environment"),
            );
            for page in visible {
                let selected = page == self.page;
                col = col.child(
                    Button::new(SharedString::from(format!("opt-nav-{}", page.index())))
                        .ghost()
                        .w_full()
                        .selected(selected)
                        .label(page.nav_label())
                        .on_click(cx.listener(move |this, _e, _window, cx| {
                            this.select_page(page, cx);
                        })),
                );
            }
            col
        }

        /// Render the selected page's controls.
        fn render_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
            match self.page {
                OptionsPage::General => self.render_general(cx).into_any_element(),
                OptionsPage::AiFeatures => self.render_ai(cx).into_any_element(),
                OptionsPage::Generator => self.render_generator(cx).into_any_element(),
            }
        }

        fn render_general(&self, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui_component::checkbox::Checkbox;
            gpui_component::v_flex()
                .gap_3()
                .flex_1()
                .child(group_label("Refresh Rate", cx))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "Default {REFRESH_DEFAULT} ms (current {} ms)",
                            self.result.refresh_ms
                        )),
                )
                .child(group_label("Visual Experience", cx))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().foreground)
                        .child(format!(
                            "Theme #{}  ·  Font: {}",
                            self.result.theme_index,
                            if self.result.font_name.is_empty() {
                                FONT_CHOICES[0]
                            } else {
                                self.result.font_name.as_str()
                            }
                        )),
                )
                .when(!self.themes.is_empty(), |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{} themes available", self.themes.len())),
                    )
                })
                .child(
                    Checkbox::new("opt-titlecase")
                        .label("Uppercase menu items")
                        .checked(self.result.menu_bar_title_case)
                        .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                            this.result.menu_bar_title_case = *checked;
                            cx.notify();
                        })),
                )
                .child(
                    Checkbox::new("opt-showicon")
                        .label("Show icon in title bar")
                        .checked(self.result.show_icon)
                        .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                            this.result.show_icon = *checked;
                            cx.notify();
                        })),
                )
                .child(
                    Checkbox::new("opt-bracewrap")
                        .label("Opening brace on new line")
                        .checked(self.result.brace_wrap)
                        .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                            this.result.brace_wrap = *checked;
                            cx.notify();
                        })),
                )
        }

        fn render_ai(&self, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui_component::checkbox::Checkbox;
            gpui_component::v_flex()
                .gap_3()
                .flex_1()
                .child(group_label("MCP Server", cx))
                .child(
                    Checkbox::new("opt-automcp")
                        .label("Auto-start MCP server")
                        .checked(self.result.auto_start_mcp)
                        .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                            this.result.auto_start_mcp = *checked;
                            cx.notify();
                        })),
                )
        }

        fn render_generator(&self, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui_component::checkbox::Checkbox;
            gpui_component::v_flex()
                .gap_3()
                .flex_1()
                .child(group_label("C++ Header", cx))
                .child(
                    Checkbox::new("opt-asserts")
                        .label("Emit static_assert size checks")
                        .checked(self.result.generator_asserts)
                        .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                            this.result.generator_asserts = *checked;
                            cx.notify();
                        })),
                )
        }
    }

    /// A group-box-style section title.
    fn group_label(text: &str, cx: &mut Context<OptionsDialog>) -> impl IntoElement {
        div()
            .text_sm()
            .font_weight(FontWeight::BOLD)
            .text_color(cx.theme().foreground)
            .child(text.to_string())
    }

    impl Focusable for OptionsDialog {
        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.focus_handle.clone()
        }
    }

    impl EventEmitter<OptionsEvent> for OptionsDialog {}

    impl Render for OptionsDialog {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui_component::button::{Button, ButtonVariants as _};
            let nav = self.render_nav(cx);
            let page = self.render_page(cx);

            gpui_component::v_flex()
                .id("rcx-options-dialog")
                .track_focus(&self.focus_handle)
                .key_context("RcxOptions")
                .w(px(700.))
                .h(px(450.))
                .p_3()
                .gap_3()
                .bg(cx.theme().background)
                .child(
                    gpui_component::h_flex()
                        .flex_1()
                        .min_h_0()
                        .gap_3()
                        .child(
                            gpui_component::v_flex()
                                .w(px(200.))
                                .gap_2()
                                .child(Input::new(&self.search).w_full())
                                .child(nav),
                        )
                        .child(div().flex_1().min_w_0().child(page)),
                )
                .child(
                    gpui_component::h_flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("opt-cancel")
                                .label("Cancel")
                                .on_click(cx.listener(|this, _e, _window, cx| this.cancel(cx))),
                        )
                        .child(
                            Button::new("opt-ok")
                                .primary()
                                .label("OK")
                                .on_click(cx.listener(|this, _e, _window, cx| this.confirm(cx))),
                        ),
                )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        filter_visible, OptionsPage, OptionsResult, FONT_CHOICES, REFRESH_MAX, REFRESH_MIN,
    };

    #[test]
    fn default_result_matches_cpp_member_values() {
        let r = OptionsResult::default();
        assert_eq!(r.theme_index, 0);
        assert!(r.menu_bar_title_case);
        assert!(!r.show_icon);
        assert!(r.auto_start_mcp);
        assert_eq!(r.refresh_ms, 660);
        assert!(!r.generator_asserts);
        assert!(!r.brace_wrap);
    }

    #[test]
    fn three_font_choices_per_source() {
        // The source adds 3 fonts (the stale test asserts 2; §24 Q1).
        assert_eq!(FONT_CHOICES.len(), 3);
        assert_eq!(FONT_CHOICES[0], "IBM Plex Mono");
        assert_eq!(FONT_CHOICES[1], "JetBrains Mono");
        assert_eq!(FONT_CHOICES[2], "Consolas");
    }

    #[test]
    fn three_pages_in_nav_order() {
        assert_eq!(OptionsPage::ALL.len(), 3);
        assert_eq!(OptionsPage::ALL[0].index(), 0);
        assert_eq!(OptionsPage::ALL[1].index(), 1);
        assert_eq!(OptionsPage::ALL[2].index(), 2);
        assert_eq!(OptionsPage::General.nav_label(), "General");
        assert_eq!(OptionsPage::AiFeatures.nav_label(), "AI Features");
        assert_eq!(OptionsPage::Generator.nav_label(), "Generator");
    }

    #[test]
    fn empty_query_shows_all_pages() {
        assert_eq!(filter_visible("").len(), 3);
        assert_eq!(filter_visible("   ").len(), 3);
    }

    #[test]
    fn mcp_search_hides_general_shows_ai() {
        // "MCP" search hides General, shows AI Features (test_options_dialog.cpp:201).
        let visible = filter_visible("MCP");
        assert!(visible.contains(&OptionsPage::AiFeatures));
        assert!(!visible.contains(&OptionsPage::General));
        // Clearing un-hides.
        assert_eq!(filter_visible("").len(), 3);
    }

    #[test]
    fn search_matches_page_label() {
        let visible = filter_visible("generator");
        assert_eq!(visible, vec![OptionsPage::Generator]);
    }

    #[test]
    fn search_matches_keyword_case_insensitively() {
        // "static_assert" is a keyword of the Generator page.
        let visible = filter_visible("STATIC_ASSERT");
        assert_eq!(visible, vec![OptionsPage::Generator]);
        // A General-page keyword.
        let visible = filter_visible("uppercase");
        assert_eq!(visible, vec![OptionsPage::General]);
    }

    #[test]
    fn refresh_clamps_zero_to_min() {
        // Spin clamps 0 → 1 (test_options_dialog.cpp:263).
        let mut r = OptionsResult::default();
        r.refresh_ms = 0;
        r.clamp_refresh();
        assert_eq!(r.refresh_ms, REFRESH_MIN);
        // And caps at the max.
        r.refresh_ms = 999_999;
        r.clamp_refresh();
        assert_eq!(r.refresh_ms, REFRESH_MAX);
    }
}
