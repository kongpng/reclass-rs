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
//! - [`filter_visible`] / [`filter_visible_with_themes`] — the recursive tree
//!   search filter (`filterTree`), unit-tested against the C++ rule (name OR
//!   page-keywords OR any child); the themes-aware variant also matches the
//!   General page against the live `m_themeCombo` item texts.
//! - [`FONT_CHOICES`] — the two font combo items in C++ order
//!   (`optionsdialog.cpp:111-112`): JetBrains Mono, then Consolas.
//! - [`parse_refresh_ms`] / [`font_choice_index`] — the General-page control
//!   value reducers (the refresh spinbox parse+clamp and the font-combo index),
//!   unit-tested headlessly.
//! - [`OptionsDialog`] / [`OptionsEvent`] — the gpui view raising `Apply(result)`
//!   / `Cancel`, with real interactive Theme/Font dropdowns and an editable
//!   refresh-rate (ms) input on the General page, plus a generator-page deep
//!   link ([`OptionsDialog::view_on_page`]).
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
/// The value cited in the refresh-spin **description** text (660 ms). This is the
/// `OptionsResult::refreshMs` struct-member default (`optionsdialog.h:19`), but it
/// is **not** the live fresh-install seed — see [`REFRESH_FALLBACK`]. Kept only so
/// the description string and the struct default stay in sync.
pub const REFRESH_DEFAULT: i32 = 660;
/// The fresh-install fallback when the `refreshMs` QSettings key is **unset**
/// (`kDefaultRefreshMs = 200`, `core.h:1224`). Both `main.cpp:5024` (the Options
/// dialog seed) and `controller.cpp:5511` (the live refresh timer) read
/// `value("refreshMs", kDefaultRefreshMs)`, so with no persisted key Options shows
/// 200 and the controllers tick at 200. The 660 above is a dead struct-field
/// default that the live code path never uses as the unset-key seed.
pub const REFRESH_FALLBACK: i32 = 200;

/// The C++ refresh-spin description (`optionsdialog.cpp:87-89`), shown under the
/// "Interval:" spinbox on the General page.
pub const REFRESH_DESC: &str =
    "How often live memory is re-read and the view is updated, in milliseconds. \
     Lower values give faster updates but use more CPU. Default: 660 ms.";

/// The verbatim C++ AI-page MCP description (`optionsdialog.cpp:152-154`), shown
/// under the "Auto-start MCP server" checkbox. The two adjacent C++ string
/// literals concatenate (with the space the first literal ends on) into one line.
pub const MCP_DESC: &str =
    "Automatically start the MCP bridge server when the application launches, \
     allowing external AI tools to connect and interact with the editor.";

/// The font combo items (`optionsdialog.cpp:111-112`) — exactly two, in source
/// order: the C++ ctor calls `m_fontCombo->addItem("JetBrains Mono")` then
/// `addItem("Consolas")` and nothing else. (An earlier port added a third
/// "IBM Plex Mono" row, but that font cannot be represented in the View ▸ Font
/// submenu — which lists only Consolas / JetBrains Mono — so picking it left the
/// menu ✓ inconsistent. We match the C++ two-item list.)
pub const FONT_CHOICES: [&str; 2] = ["JetBrains Mono", "Consolas"];

/// Parse a refresh-rate edit string into a clamped ms value
/// (`m_refreshSpin`): keep the leading run of ASCII digits (the C++ spinbox only
/// accepts digits; a trailing `" ms"` suffix or stray text is ignored), parse,
/// then clamp to `1..=60000`. An empty / all-non-digit string clamps to the min.
pub fn parse_refresh_ms(text: &str) -> i32 {
    let digits: String = text
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits
        .parse::<i64>()
        .unwrap_or(REFRESH_MIN as i64)
        .clamp(REFRESH_MIN as i64, REFRESH_MAX as i64) as i32
}

/// The index of `font_name` within [`FONT_CHOICES`] (the combo's current row);
/// an empty/unknown font falls back to the first item, matching the C++
/// `setCurrentText` (which leaves the combo on its first item when the stored
/// font is not one of the listed choices).
pub fn font_choice_index(font_name: &str) -> usize {
    FONT_CHOICES
        .iter()
        .position(|f| f.eq_ignore_ascii_case(font_name))
        .unwrap_or(0)
}

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
    /// item under the page (`collectPageKeywords`, `optionsdialog.cpp:234-249`).
    /// Drives the tree search filter so e.g. "MCP" surfaces the AI Features page.
    ///
    /// This mirrors the C++ exactly: `collectPageKeywords` walks
    /// `page->findChildren<QWidget*>()` and gathers every `QLabel->text()`,
    /// `QCheckBox->text()`, `QGroupBox->title()`, and `QComboBox` item text. A
    /// `QFormLayout::addRow("Label:", widget)` creates a `QLabel` child, so the
    /// `"Interval:"`/`"Color theme:"`/`"Editor Font:"` field captions and the two
    /// long description labels ([`REFRESH_DESC`]/[`MCP_DESC`]) are collected too.
    ///
    /// The one keyword set this static list *cannot* hold is the General page's
    /// theme-combo item texts (the live `ThemeManager` theme names), which the C++
    /// collects from `m_themeCombo`. Those are folded into the search via
    /// [`filter_visible_with_themes`] / [`page_matches_themes`], which feed the
    /// live `self.themes` list in addition to these static keywords.
    pub fn keywords(self) -> Vec<&'static str> {
        match self {
            OptionsPage::General => {
                let mut kw = vec![
                    // QGroupBox "Refresh Rate" (line 74).
                    "Refresh Rate",
                    // QLabel "Interval:" (addRow, line 85).
                    "Interval:",
                    // QLabel refreshDesc (lines 87-89).
                    REFRESH_DESC,
                    // QGroupBox "Visual Experience" (line 97).
                    "Visual Experience",
                    // QLabel "Color theme:" (addRow, line 108).
                    "Color theme:",
                    // QLabel "Editor Font:" (addRow, line 115).
                    "Editor Font:",
                    // QCheckBox texts (lines 117/121/125).
                    "Uppercase menu items",
                    "Show icon in title bar",
                    "Opening brace on new line",
                ];
                // m_fontCombo item texts (lines 111-112). (m_themeCombo item texts
                // are dynamic — see [`page_matches_themes`].)
                kw.extend(FONT_CHOICES);
                kw
            }
            // QGroupBox "MCP Server" (line 144), QCheckBox "Auto-start MCP server"
            // (line 148), QLabel mcpDesc (lines 152-154).
            OptionsPage::AiFeatures => vec!["MCP Server", "Auto-start MCP server", MCP_DESC],
            // QGroupBox "C++ Header" (line 174), QCheckBox text (line 176).
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
///
/// This zero-arg variant uses the static [`keywords`](OptionsPage::keywords) only;
/// it does **not** know the live theme names. Use [`filter_visible_with_themes`]
/// (passing the dialog's `self.themes`) to also match the General page against the
/// dynamic `m_themeCombo` item texts, as the C++ `collectPageKeywords` does.
pub fn filter_visible(query: &str) -> Vec<OptionsPage> {
    filter_visible_with_themes(query, &[])
}

/// [`filter_visible`] but also testing the General page against the live theme
/// names — the C++ `m_themeCombo` item texts that `collectPageKeywords` folds into
/// `m_pageKeywords[generalItem]`. `themes` is the dialog's available theme list.
pub fn filter_visible_with_themes(query: &str, themes: &[String]) -> Vec<OptionsPage> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return OptionsPage::ALL.to_vec();
    }
    OptionsPage::ALL
        .iter()
        .copied()
        .filter(|p| page_matches_themes(*p, &q, themes))
        .collect()
}

/// Step the nav-tree selection to the next/previous **visible** page (the C++
/// `QTreeWidget` Up/Down across the filtered tree). `pages` is the currently
/// visible page list ([`filter_visible`]); `current` is the selected page;
/// `delta` is +1 (Down) or -1 (Up). Returns the page to select next, clamped to
/// the ends (no wraparound, matching the tree's keyboard navigation). Returns
/// `None` when there is nothing to move to (empty list, or the current page is
/// not visible).
pub fn step_visible_page(
    pages: &[OptionsPage],
    current: OptionsPage,
    delta: isize,
) -> Option<OptionsPage> {
    if pages.is_empty() {
        return None;
    }
    let cur = pages.iter().position(|p| *p == current)?;
    let last = pages.len() - 1;
    let next = (cur as isize + delta).clamp(0, last as isize) as usize;
    Some(pages[next])
}

/// Whether a page matches the (already-lowercased, trimmed) query, additionally
/// testing the General page against the live theme names (the dynamic
/// `m_themeCombo` item texts). `themes` only affects the General page (the only
/// page hosting a theme combo in the C++); the other pages ignore it.
fn page_matches_themes(page: OptionsPage, q_lower: &str, themes: &[String]) -> bool {
    if page.nav_label().to_lowercase().contains(q_lower) {
        return true;
    }
    if page
        .keywords()
        .iter()
        .any(|kw| kw.to_lowercase().contains(q_lower))
    {
        return true;
    }
    // The General page also carries the live theme-combo item texts as keywords.
    if page == OptionsPage::General {
        return themes
            .iter()
            .any(|name| name.to_lowercase().contains(q_lower));
    }
    false
}

// ── gpui view ───────────────────────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use view::{OptionsDialog, OptionsEvent};

#[cfg(feature = "ui")]
mod view {
    use super::{
        filter_visible_with_themes, font_choice_index, parse_refresh_ms, step_visible_page,
        OptionsPage, OptionsResult, FONT_CHOICES, MCP_DESC, REFRESH_DESC,
    };
    use crate::ui::design::{color, section_label, tokens, zed_list_row};
    use crate::ui::dialogs::modal;
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::button::{Button, ButtonVariants as _};
    use gpui_component::input::{Input, InputEvent, InputState};
    use gpui_component::popover::Popover;
    use gpui_component::Sizable as _;

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
        /// The editable refresh-rate (ms) input (`m_refreshSpin`); its value is
        /// parsed + clamped back into `result.refresh_ms` on every change.
        refresh: Entity<InputState>,
        /// Whether the Theme / Font dropdown popovers are open.
        theme_open: bool,
        font_open: bool,
        /// First-letter type-ahead buffer for the open combo (the C++ QComboBox
        /// keyboard selection) + the last keystroke time; the buffer resets after a
        /// short pause so a fresh prefix isn't appended to a stale one.
        type_ahead: String,
        type_ahead_at: Option<std::time::Instant>,
        focus_handle: FocusHandle,
        _subscriptions: Vec<Subscription>,
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
            let refresh = {
                let initial = current.refresh_ms.to_string();
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder("660")
                        .default_value(initial)
                })
            };
            let mut subs = Vec::new();
            subs.push(cx.subscribe_in(
                &search,
                window,
                |this, _s, ev: &InputEvent, _window, cx| {
                    if matches!(ev, InputEvent::Change) {
                        this.query = this.search.read(cx).value().to_string();
                        // If the selected page is filtered out, jump to the first
                        // visible page (the C++ tree hides non-matching items).
                        let visible = filter_visible_with_themes(&this.query, &this.themes);
                        if !visible.is_empty() && !visible.contains(&this.page) {
                            this.page = visible[0];
                        }
                        cx.notify();
                    }
                },
            ));
            subs.push(cx.subscribe_in(
                &refresh,
                window,
                |this, _s, ev: &InputEvent, _window, cx| {
                    if matches!(ev, InputEvent::Change) {
                        let raw = this.refresh.read(cx).value().to_string();
                        this.result.refresh_ms = parse_refresh_ms(&raw);
                        cx.notify();
                    }
                },
            ));
            OptionsDialog {
                result: current,
                themes,
                page: OptionsPage::General,
                search,
                query: String::new(),
                refresh,
                theme_open: false,
                font_open: false,
                type_ahead: String::new(),
                type_ahead_at: None,
                focus_handle: cx.focus_handle(),
                _subscriptions: subs,
            }
        }

        /// Build the dialog as an [`Entity`] opened directly on `page` — the C++
        /// `showOptionsDialog(int page)` / `selectPage` deep link (e.g. the
        /// generator UI opening Options on the Generator page).
        pub fn view_on_page(
            current: OptionsResult,
            themes: Vec<String>,
            page: OptionsPage,
            window: &mut Window,
            cx: &mut App,
        ) -> Entity<Self> {
            cx.new(|cx| {
                let mut this = OptionsDialog::new(current, themes, window, cx);
                this.page = page;
                this
            })
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

        /// Whether a text input (search or refresh) currently holds focus — used to
        /// guard Enter so it does not fire OK while the user is typing in a field.
        fn an_input_focused(&self, window: &Window, cx: &App) -> bool {
            self.search.read(cx).focus_handle(cx).is_focused(window)
                || self.refresh.read(cx).focus_handle(cx).is_focused(window)
        }

        /// Move the nav-tree selection to the next/previous visible page (Up/Down).
        fn step_page(&mut self, delta: isize, cx: &mut Context<Self>) {
            let visible = filter_visible_with_themes(&self.query, &self.themes);
            if let Some(next) = step_visible_page(&visible, self.page, delta) {
                self.select_page(next, cx);
            }
        }

        /// Capture-phase key handling (the C++ `ThemedDialog` accept/reject + the
        /// `Ctrl+E` search shortcut + the nav-tree Up/Down): Escape cancels; Enter
        /// confirms (OK) unless a dropdown is open or a text input owns focus (so
        /// typing/picking isn't hijacked); Ctrl+E focuses the search box; Up/Down
        /// walk the visible pages. Returns `true` when handled.
        fn handle_nav_key(
            &mut self,
            key: &str,
            modifiers: &Modifiers,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> bool {
            // Ctrl+E focuses the search box (the placeholder's advertised shortcut).
            if key == "e" && modifiers.control {
                let search = self.search.clone();
                window.focus(&search.read(cx).focus_handle(cx), cx);
                cx.notify();
                return true;
            }
            // While a combo dropdown is open, printable keys drive first-letter
            // type-ahead over its entries (the C++ QComboBox keyboard selection).
            if self.combo_type_ahead(key, modifiers, cx) {
                return true;
            }
            match key {
                "escape" => {
                    self.cancel(cx);
                    true
                }
                "enter" => {
                    // Don't fire OK while a dropdown is open or an input is focused
                    // (Enter there picks/commits the control, not the dialog).
                    if self.theme_open || self.font_open || self.an_input_focused(window, cx) {
                        false
                    } else {
                        self.confirm(cx);
                        true
                    }
                }
                "down" => {
                    self.step_page(1, cx);
                    true
                }
                "up" => {
                    self.step_page(-1, cx);
                    true
                }
                _ => false,
            }
        }

        /// First-letter type-ahead for the OPEN combo: accumulate the typed prefix
        /// (resetting after ~0.8s of inactivity) and move the combo's highlighted
        /// entry to the first name that matches, keeping the popover open so Enter /
        /// click still commits. Returns `true` when it consumed the key.
        fn combo_type_ahead(
            &mut self,
            key: &str,
            modifiers: &Modifiers,
            cx: &mut Context<Self>,
        ) -> bool {
            if !(self.theme_open || self.font_open) {
                return false;
            }
            // Only plain printable single characters (never a shortcut chord).
            if modifiers.control || modifiers.alt || modifiers.platform || key.chars().count() != 1 {
                return false;
            }
            let ch = key.chars().next().unwrap();
            if !ch.is_alphanumeric() {
                return false;
            }
            let now = std::time::Instant::now();
            let stale = self
                .type_ahead_at
                .map_or(true, |t| now.duration_since(t) > std::time::Duration::from_millis(800));
            if stale {
                self.type_ahead.clear();
            }
            self.type_ahead.push(ch.to_ascii_lowercase());
            self.type_ahead_at = Some(now);
            let needle = self.type_ahead.as_str();
            if self.theme_open {
                let themes: Vec<String> = if self.themes.is_empty() {
                    vec![self.theme_label()]
                } else {
                    self.themes.clone()
                };
                if let Some(ix) = themes.iter().position(|n| n.to_lowercase().starts_with(needle)) {
                    self.result.theme_index = ix;
                    cx.notify();
                }
            } else if let Some(ix) = FONT_CHOICES
                .iter()
                .position(|n| n.to_lowercase().starts_with(needle))
            {
                self.result.font_name = FONT_CHOICES[ix].to_string();
                cx.notify();
            }
            true
        }

        /// Pick a theme (the C++ `m_themeCombo->setCurrentIndex`).
        fn set_theme(&mut self, index: usize, cx: &mut Context<Self>) {
            self.result.theme_index = index;
            self.theme_open = false;
            cx.notify();
        }

        /// Pick an editor font (the C++ `m_fontCombo->setCurrentText`).
        fn set_font(&mut self, name: &str, cx: &mut Context<Self>) {
            self.result.font_name = name.to_string();
            self.font_open = false;
            cx.notify();
        }

        /// The current theme combo label (the selected theme name, or a synthetic
        /// "Theme #N" when the manager supplied no names).
        fn theme_label(&self) -> String {
            self.themes
                .get(self.result.theme_index)
                .cloned()
                .unwrap_or_else(|| format!("Theme #{}", self.result.theme_index))
        }

        /// The current font combo label (the stored font, or the first choice).
        fn font_label(&self) -> &str {
            if self.result.font_name.is_empty() {
                FONT_CHOICES[font_choice_index(&self.result.font_name)]
            } else {
                self.result.font_name.as_str()
            }
        }

        /// Render the left nav: a search box above the filtered page list, styled
        /// like a Zed settings sidebar (an "Environment" group caption + soft
        /// accent-selected rows).
        fn render_nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
            let visible = filter_visible_with_themes(&self.query, &self.themes);
            let mut list = gpui_component::v_flex()
                .w_full()
                .gap(px(tokens::space::XXS))
                .child(section_label("Environment", cx));
            for page in visible {
                let selected = page == self.page;
                list = list.child(
                    zed_list_row(
                        SharedString::from(format!("opt-nav-{}", page.index())),
                        selected,
                        cx,
                    )
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _e, _window, cx| {
                        this.select_page(page, cx);
                    }))
                    .child(page.nav_label()),
                );
            }

            gpui_component::v_flex()
                .w(px(200.))
                .flex_none()
                .h_full()
                .gap(px(tokens::space::MD))
                .pr(px(tokens::space::LG))
                .border_r_1()
                .border_color(color::border(cx))
                .child(Input::new(&self.search).w_full())
                .child(list)
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
                .gap(px(tokens::space::SM))
                .flex_1()
                // ── Refresh Rate group (the C++ `m_refreshSpin` + description) ──
                .child(section_label("Refresh Rate", cx))
                .child(
                    gpui_component::h_flex()
                        .w_full()
                        .gap(px(tokens::space::MD))
                        .items_center()
                        .child(modal::field_label("Interval:", cx))
                        .child(Input::new(&self.refresh).small().w(px(110.)))
                        .child(
                            div()
                                .text_size(px(tokens::font::UI_SM))
                                .text_color(color::text_muted(cx))
                                .child("ms"),
                        ),
                )
                .child(modal::help_text(REFRESH_DESC, cx))
                // ── Visual Experience group (theme + font combos) ──
                .child(section_label("Visual Experience", cx))
                .child(
                    gpui_component::h_flex()
                        .w_full()
                        .gap(px(tokens::space::MD))
                        .items_center()
                        .child(
                            div()
                                .w(px(96.))
                                .flex_none()
                                .child(modal::field_label("Color theme:", cx)),
                        )
                        .child(self.render_theme_combo(cx)),
                )
                .child(
                    gpui_component::h_flex()
                        .w_full()
                        .gap(px(tokens::space::MD))
                        .items_center()
                        .child(
                            div()
                                .w(px(96.))
                                .flex_none()
                                .child(modal::field_label("Editor Font:", cx)),
                        )
                        .child(self.render_font_combo(cx)),
                )
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

        /// The Theme combo (`m_themeCombo`): a Zed dropdown over the available
        /// theme names, applying the pick to `result.theme_index`.
        fn render_theme_combo(&self, cx: &mut Context<Self>) -> impl IntoElement {
            let dialog = cx.entity().downgrade();
            let current = self.result.theme_index;
            // Fall back to a single synthetic row if the manager gave no names, so
            // the control is still interactive (and the label is meaningful).
            let themes: Vec<String> = if self.themes.is_empty() {
                vec![self.theme_label()]
            } else {
                self.themes.clone()
            };
            Popover::new("opt-theme-pop")
                .anchor(Anchor::TopLeft)
                .open(self.theme_open)
                .on_open_change(cx.listener(|this, open: &bool, _w, cx| {
                    this.theme_open = *open;
                    cx.notify();
                }))
                .trigger(dropdown_trigger("opt-theme", self.theme_label()))
                .content(move |_state, _window, cx| {
                    let mut menu = dropdown_menu(cx);
                    for (i, name) in themes.iter().enumerate() {
                        let dialog = dialog.clone();
                        menu = menu.child(
                            dropdown_row(("opt-theme-row", i), name.clone(), i == current, cx)
                                .on_click(move |_e, _w, cx| {
                                    dialog.update(cx, |this, cx| this.set_theme(i, cx)).ok();
                                }),
                        );
                    }
                    menu
                })
        }

        /// The Font combo (`m_fontCombo`): a Zed dropdown over [`FONT_CHOICES`],
        /// applying the pick to `result.font_name`.
        fn render_font_combo(&self, cx: &mut Context<Self>) -> impl IntoElement {
            let dialog = cx.entity().downgrade();
            let current = font_choice_index(&self.result.font_name);
            Popover::new("opt-font-pop")
                .anchor(Anchor::TopLeft)
                .open(self.font_open)
                .on_open_change(cx.listener(|this, open: &bool, _w, cx| {
                    this.font_open = *open;
                    cx.notify();
                }))
                .trigger(dropdown_trigger("opt-font", self.font_label().to_string()))
                .content(move |_state, _window, cx| {
                    let mut menu = dropdown_menu(cx);
                    for (i, name) in FONT_CHOICES.iter().enumerate() {
                        let dialog = dialog.clone();
                        let name = *name;
                        menu = menu.child(
                            dropdown_row(("opt-font-row", i), name, i == current, cx).on_click(
                                move |_e, _w, cx| {
                                    dialog.update(cx, |this, cx| this.set_font(name, cx)).ok();
                                },
                            ),
                        );
                    }
                    menu
                })
        }

        fn render_ai(&self, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui_component::checkbox::Checkbox;
            gpui_component::v_flex()
                .gap(px(tokens::space::SM))
                .flex_1()
                .child(section_label("MCP Server", cx))
                .child(
                    Checkbox::new("opt-automcp")
                        .label("Auto-start MCP server")
                        .checked(self.result.auto_start_mcp)
                        .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                            this.result.auto_start_mcp = *checked;
                            cx.notify();
                        })),
                )
                // Verbatim C++ MCP description (optionsdialog.cpp:152-154).
                .child(modal::help_text(MCP_DESC, cx))
        }

        fn render_generator(&self, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui_component::checkbox::Checkbox;
            gpui_component::v_flex()
                .gap(px(tokens::space::SM))
                .flex_1()
                .child(section_label("C++ Header", cx))
                // The C++ `cppGroup` (optionsdialog.cpp:174-179) holds ONLY the
                // checkbox — no description label — so we add none here.
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

    impl Focusable for OptionsDialog {
        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.focus_handle.clone()
        }
    }

    impl EventEmitter<OptionsEvent> for OptionsDialog {}

    impl Render for OptionsDialog {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let nav = self.render_nav(cx);
            let page = self.render_page(cx);
            // Clamp the design 720×480 card to the live window so it never spills
            // off the right edge (and its footer buttons stay reachable) when the
            // window is narrower/shorter than the design size (QA #1).
            let card_w = modal::clamp_width(720., window);
            let card_h = modal::clamp_height(480., 48., window);

            let body = modal::body(cx).child(
                gpui_component::h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .gap(px(tokens::space::LG))
                    .child(nav)
                    .child(
                        div()
                            .id("rcx-options-page")
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_y_scroll()
                            .child(page),
                    ),
            );

            let footer = modal::footer(cx)
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
                );

            modal::card(cx)
                .id("rcx-options-dialog")
                .track_focus(&self.focus_handle)
                .key_context("RcxOptions")
                // Capture-phase key handling so Escape/Enter/Ctrl+E/Up-Down reach
                // the dialog even while the search/refresh input owns focus (the C++
                // ThemedDialog accept/reject + Ctrl+E + nav-tree navigation).
                .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                    if this.handle_nav_key(
                        ev.keystroke.key.as_str(),
                        &ev.keystroke.modifiers,
                        window,
                        cx,
                    ) {
                        cx.stop_propagation();
                    }
                }))
                .w(card_w)
                .h(card_h)
                .child(modal::header("Options", cx).child(modal::close_button(
                    "opt-close",
                    cx.listener(|this, _e, _window, cx| this.cancel(cx)),
                    cx,
                )))
                .child(body)
                .child(footer)
        }
    }

    // ── Zed dropdown scaffolding (mirrors the scanner-panel combos) ──────────

    /// A compact Zed dropdown trigger: an outline button showing the current pick
    /// with a trailing chevron glyph (the app does not bundle the gpui-component
    /// SVG icon assets, so a `"⌄"` glyph reads as the combo affordance with no
    /// missing-asset box).
    fn dropdown_trigger(id: impl Into<SharedString>, text: impl Into<SharedString>) -> Button {
        let id: SharedString = id.into();
        let text: SharedString = text.into();
        Button::new(SharedString::from(format!("opt-trig-{id}")))
            .outline()
            .small()
            .label(SharedString::from(format!("{text}  \u{2304}")))
    }

    /// The elevated container the dropdown popovers drop into (§5.10 menu surface).
    fn dropdown_menu(cx: &App) -> Div {
        gpui_component::v_flex()
            .min_w(px(160.))
            .p(px(tokens::space::XS))
            .gap(px(1.))
            .bg(color::elevated_bg(cx))
            .border_1()
            .border_color(color::border(cx))
            .rounded(px(tokens::radius::LG))
            .shadow_md()
    }

    /// A clickable inset dropdown row with the soft-accent fill on the current pick.
    fn dropdown_row(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        selected: bool,
        cx: &App,
    ) -> Stateful<Div> {
        let label: SharedString = label.into();
        gpui_component::h_flex()
            .id(id)
            .w_full()
            .h(px(24.))
            .px(px(tokens::space::MD))
            .items_center()
            .rounded(px(tokens::radius::MD))
            .text_size(px(tokens::font::UI_MD))
            .text_color(color::text(cx))
            .cursor_pointer()
            .when(selected, |r| r.bg(color::selected_bg(cx)))
            .when(!selected, |r| r.hover(|s| s.bg(color::hover_overlay(cx))))
            .child(label)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        filter_visible, filter_visible_with_themes, font_choice_index, parse_refresh_ms,
        step_visible_page, OptionsPage, OptionsResult, FONT_CHOICES, MCP_DESC, REFRESH_DEFAULT,
        REFRESH_FALLBACK, REFRESH_MAX, REFRESH_MIN,
    };

    #[test]
    fn default_result_matches_cpp_member_values() {
        let r = OptionsResult::default();
        assert_eq!(r.theme_index, 0);
        assert!(r.menu_bar_title_case);
        assert!(!r.show_icon);
        assert!(r.auto_start_mcp);
        // The OptionsResult struct-member default (optionsdialog.h:19) is 660.
        assert_eq!(r.refresh_ms, REFRESH_DEFAULT);
        assert_eq!(r.refresh_ms, 660);
        assert!(!r.generator_asserts);
        assert!(!r.brace_wrap);
    }

    #[test]
    fn fresh_install_refresh_fallback_is_kdefaultrefreshms_200() {
        // The unset-`refreshMs`-key fallback must be kDefaultRefreshMs = 200
        // (core.h:1224), the value both main.cpp:5024 (Options seed) and
        // controller.cpp:5511 (refresh timer) read. The 660 struct-member default
        // is NOT the live fresh-install seed.
        assert_eq!(REFRESH_FALLBACK, 200);
        assert_ne!(REFRESH_FALLBACK, REFRESH_DEFAULT);
    }

    #[test]
    fn two_font_choices_in_cpp_order() {
        // The C++ ctor adds exactly two fonts, in this order
        // (optionsdialog.cpp:111-112): JetBrains Mono, then Consolas. A prior
        // port added a third ("IBM Plex Mono") that the View ▸ Font submenu
        // (Consolas / JetBrains Mono only) could not represent; this pins the
        // corrected, source-faithful two-item list.
        assert_eq!(FONT_CHOICES.len(), 2);
        assert_eq!(FONT_CHOICES[0], "JetBrains Mono");
        assert_eq!(FONT_CHOICES[1], "Consolas");
        // The dropped font is no longer selectable.
        assert!(!FONT_CHOICES.contains(&"IBM Plex Mono"));
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
    fn general_field_caption_keywords_surface_general() {
        // The QFormLayout addRow captions ("Interval:"/"Color theme:"/"Editor
        // Font:") are QLabel children collected by collectPageKeywords, so each
        // surfaces the General page (optionsdialog.cpp:85/108/115).
        for q in ["interval", "color", "theme", "editor", "font"] {
            let visible = filter_visible(q);
            assert!(
                visible.contains(&OptionsPage::General),
                "query {q:?} should surface General"
            );
            assert!(
                !visible.contains(&OptionsPage::Generator),
                "query {q:?} should NOT surface Generator"
            );
        }
        // "color"/"theme"/"interval"/"font" are unique to General.
        for q in ["color", "theme", "interval", "font"] {
            assert_eq!(filter_visible(q), vec![OptionsPage::General], "query {q:?}");
        }
        // NB: "editor" ALSO surfaces AI Features — the C++ mcpDesc ends "...interact
        // with the editor." So the substring match (collectPageKeywords gathers
        // QLabel text) hits both pages, faithfully (optionsdialog.cpp:154).
        let visible = filter_visible("editor");
        assert!(visible.contains(&OptionsPage::General));
        assert!(visible.contains(&OptionsPage::AiFeatures));
    }

    #[test]
    fn refresh_desc_words_surface_general() {
        // The refresh description QLabel (REFRESH_DESC) is a General keyword, so
        // words unique to it ("milliseconds", "cpu") surface General and nothing
        // else (optionsdialog.cpp:87-89).
        for q in ["milliseconds", "cpu"] {
            let visible = filter_visible(q);
            assert_eq!(visible, vec![OptionsPage::General], "query {q:?}");
        }
    }

    #[test]
    fn mcp_desc_words_surface_ai_only() {
        // The MCP description QLabel (MCP_DESC) is an AI-page keyword, so its
        // distinctive phrasing surfaces AI Features only (optionsdialog.cpp:152-154).
        let visible = filter_visible("bridge");
        assert_eq!(visible, vec![OptionsPage::AiFeatures]);
        let visible = filter_visible("external AI");
        assert_eq!(visible, vec![OptionsPage::AiFeatures]);
    }

    #[test]
    fn interval_query_hides_ai_and_generator() {
        // A General-only term must hide the other two pages (the C++ tree hides
        // non-matching items).
        let visible = filter_visible("interval");
        assert_eq!(visible, vec![OptionsPage::General]);
        assert!(!visible.contains(&OptionsPage::AiFeatures));
        assert!(!visible.contains(&OptionsPage::Generator));
    }

    #[test]
    fn theme_name_search_surfaces_general_via_themes() {
        // The live m_themeCombo item texts are folded into the General page's
        // keyword set (collectPageKeywords gathers QComboBox itemText). A theme
        // name only matches when fed through the themes-aware filter.
        let themes = vec!["Midnight Ocean".to_string(), "Solarized Light".to_string()];
        let visible = filter_visible_with_themes("ocean", &themes);
        assert_eq!(visible, vec![OptionsPage::General]);
        // "solarized" likewise — and only General (theme combo is General-only).
        let visible = filter_visible_with_themes("solarized", &themes);
        assert_eq!(visible, vec![OptionsPage::General]);
        // Without the themes the same query matches no page (the static keyword
        // list has no theme names).
        assert!(filter_visible("ocean").is_empty());
        // A theme name does not leak into AI/Generator matching.
        let visible = filter_visible_with_themes("ocean", &themes);
        assert!(!visible.contains(&OptionsPage::AiFeatures));
        assert!(!visible.contains(&OptionsPage::Generator));
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

    #[test]
    fn parse_refresh_ms_reads_digits_and_clamps() {
        // Plain digits.
        assert_eq!(parse_refresh_ms("250"), 250);
        // A trailing " ms" suffix (the spinbox display) is ignored.
        assert_eq!(parse_refresh_ms("660 ms"), 660);
        assert_eq!(parse_refresh_ms("  120  "), 120);
        // Below/above the range clamps (the QSpinBox range 1..=60000).
        assert_eq!(parse_refresh_ms("0"), REFRESH_MIN);
        assert_eq!(parse_refresh_ms("999999"), REFRESH_MAX);
        // Empty / non-numeric → the minimum (defensive, like an empty spinbox).
        assert_eq!(parse_refresh_ms(""), REFRESH_MIN);
        assert_eq!(parse_refresh_ms("abc"), REFRESH_MIN);
    }

    #[test]
    fn step_visible_page_walks_and_clamps() {
        let all = OptionsPage::ALL.to_vec();
        // Down from General → AI Features → Generator, clamps at the end.
        assert_eq!(
            step_visible_page(&all, OptionsPage::General, 1),
            Some(OptionsPage::AiFeatures)
        );
        assert_eq!(
            step_visible_page(&all, OptionsPage::AiFeatures, 1),
            Some(OptionsPage::Generator)
        );
        assert_eq!(
            step_visible_page(&all, OptionsPage::Generator, 1),
            Some(OptionsPage::Generator)
        );
        // Up clamps at the start.
        assert_eq!(
            step_visible_page(&all, OptionsPage::Generator, -1),
            Some(OptionsPage::AiFeatures)
        );
        assert_eq!(
            step_visible_page(&all, OptionsPage::General, -1),
            Some(OptionsPage::General)
        );
    }

    #[test]
    fn step_visible_page_respects_filtered_list() {
        // With only AI + Generator visible, Up from Generator lands on AI Features
        // (General is hidden, so it is skipped).
        let visible = vec![OptionsPage::AiFeatures, OptionsPage::Generator];
        assert_eq!(
            step_visible_page(&visible, OptionsPage::Generator, -1),
            Some(OptionsPage::AiFeatures)
        );
        // A current page that is not visible → no move.
        assert_eq!(step_visible_page(&visible, OptionsPage::General, 1), None);
        // Empty list → no move.
        assert_eq!(step_visible_page(&[], OptionsPage::General, 1), None);
    }

    #[test]
    fn mcp_desc_is_verbatim_cpp() {
        // The AI-page MCP help must be the verbatim C++ string
        // (optionsdialog.cpp:152-154), not a rephrase. The two adjacent C++
        // literals concatenate to a single line with exactly one space at the
        // seam ("launches, allowing").
        assert_eq!(
            MCP_DESC,
            "Automatically start the MCP bridge server when the application \
             launches, allowing external AI tools to connect and interact with \
             the editor."
        );
        // The seam joins with exactly one space (no doubled/zero spaces).
        assert!(MCP_DESC.contains("launches, allowing external"));
        // The earlier rephrase wording is gone.
        assert!(!MCP_DESC.contains("Model-Context-Protocol"));
        assert!(!MCP_DESC.contains("Reclass launches"));
    }

    #[test]
    fn font_choice_index_resolves_or_falls_back() {
        // Two-item list, C++ order: JetBrains Mono (0), Consolas (1).
        assert_eq!(font_choice_index("JetBrains Mono"), 0);
        assert_eq!(font_choice_index("Consolas"), 1);
        // Case-insensitive (the combo stores the display text).
        assert_eq!(font_choice_index("consolas"), 1);
        assert_eq!(font_choice_index("jetbrains mono"), 0);
        // Empty / unknown → first item (the C++ combo default, JetBrains Mono).
        assert_eq!(font_choice_index(""), 0);
        assert_eq!(font_choice_index("Comic Sans"), 0);
        // The previously-listed IBM Plex Mono is no longer a valid choice → first.
        assert_eq!(font_choice_index("IBM Plex Mono"), 0);
    }
}
