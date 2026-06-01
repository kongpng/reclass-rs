//! Start page — the VS2022-style welcome overlay (app-shell §13 `startpage.h`).
//!
//! Port of `StartPageWidget`. The C++ is a fully custom-painted frameless dialog
//! shown ~90%×85% over the window: a "Reclass" title, an **Open recent** file
//! list bucketed by modified date, a column of **action cards** (New Class /
//! Open project / Import from Source / Import ReClass XML / Import PDB), a
//! "Tutorial →" link, a search filter, and ESC / outside-click dismiss. The
//! cookbook (ARCHITECTURE §5) maps it onto plain `div`/flex + buttons; the
//! bucketing + card data are pure and unit-tested headlessly.
//!
//! Split:
//! - [`RecentEntry`] / [`Bucket`] / [`bucket_for`] / [`build_groups`] — the
//!   gpui-free data model (recent files → date-bucketed groups + the search
//!   filter), mirroring `loadEntries` / `buildGroups` (`startpage.h:217-261`).
//! - [`StartCard`] — the five action cards (`drawCards`), as data.
//! - [`StartPageEvent`] / [`StartPage`] — the gpui view raising the same signals
//!   the C++ emitted (`openProject` / `newClass` / `dismissed` / `importSource`
//!   / `importXml` / `importPdb` / `fileSelected`).
//!
//! The "preload a New Class behind the splash" reflex (the window opens a fresh
//! document so dismissing lands on something; app-shell §13/§20) lives in the
//! window wiring, not here.
//!
//! Gated behind the `ui` feature.

use crate::ui::design::{color, tokens};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{Input, InputState};

/// The five start-page action cards (`drawCards`, `startpage.h`) — the order is
/// load-bearing for parity (card index → action; `hitTest` cards 0..4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StartCard {
    /// Card 0 — create a new class (`newClass`).
    NewClass,
    /// Card 1 — open a `.rcx` project (`openProject`).
    OpenProject,
    /// Card 2 — import from C/C++ source (`importSource`).
    ImportSource,
    /// Card 3 — import a ReClass XML (`importXml`).
    ImportXml,
    /// Card 4 — import a PDB (`importPdb`).
    ImportPdb,
}

impl StartCard {
    /// The five cards in display/hit-test order.
    pub const ALL: [StartCard; 5] = [
        StartCard::NewClass,
        StartCard::OpenProject,
        StartCard::ImportSource,
        StartCard::ImportXml,
        StartCard::ImportPdb,
    ];

    /// The card title (the bold line; `drawCards`).
    pub fn title(self) -> &'static str {
        match self {
            StartCard::NewClass => "New Class",
            StartCard::OpenProject => "Open project",
            StartCard::ImportSource => "Import from Source",
            StartCard::ImportXml => "Import ReClass XML",
            StartCard::ImportPdb => "Import PDB",
        }
    }

    /// The card description (the dim second line; `drawCards`).
    pub fn description(self) -> &'static str {
        match self {
            StartCard::NewClass => "Start a fresh struct layout",
            StartCard::OpenProject => "Open an existing .rcx project",
            StartCard::ImportSource => "Parse C/C++ structs from source",
            StartCard::ImportXml => "Import a ReClass.NET XML file",
            StartCard::ImportPdb => "Import types from a PDB",
        }
    }

    /// A stable element id fragment for the card button.
    fn id(self) -> &'static str {
        match self {
            StartCard::NewClass => "card-new-class",
            StartCard::OpenProject => "card-open-project",
            StartCard::ImportSource => "card-import-source",
            StartCard::ImportXml => "card-import-xml",
            StartCard::ImportPdb => "card-import-pdb",
        }
    }

    /// The leading glyph shown in the card's icon tile. SVG assets are not yet
    /// wired (see `titlebar::source_icon`), so — like the rest of the chrome — we
    /// stand in with a themeable unicode glyph that mirrors the C++ card icons
    /// (`symbol-structure` / `folder-opened` / `file-binary` / `code` / `debug`).
    fn glyph(self) -> &'static str {
        match self {
            StartCard::NewClass => "\u{25A4}",          // ▤ struct/class
            StartCard::OpenProject => "\u{1F5C1}",      // 🗁 open folder
            StartCard::ImportSource => "\u{1F5CE}",     // 🗎 source file
            StartCard::ImportXml => "\u{2039}\u{203A}", // ‹› markup
            StartCard::ImportPdb => "\u{25A3}",         // ▣ symbols
        }
    }

    /// The right-aligned keybinding hint shown for the card (Zed's welcome tab
    /// lists the conventional shortcut next to each primary action). `None` for
    /// the import actions, which the C++ reached via menus only.
    fn shortcut(self) -> Option<&'static str> {
        match self {
            StartCard::NewClass => Some("Ctrl N"),
            StartCard::OpenProject => Some("Ctrl O"),
            _ => None,
        }
    }
}

/// A recent-files entry (`struct Entry`, `startpage.h:217`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RecentEntry {
    /// Full path on disk.
    pub path: String,
    /// File name (the bold part of the row).
    pub file_name: String,
    /// Directory path (the dim, middle-elided part).
    pub dir_path: String,
    /// Days since modification (0 = today). The C++ used a `QDateTime`; the pure
    /// model takes the already-computed day delta so it stays clock-free.
    pub age_days: i64,
    /// Whether this entry is a bundled example (`isExample`).
    pub is_example: bool,
}

/// The date buckets recent files fall into (`buildGroups`, `startpage.h:236-261`).
/// Order matters — groups render in this order.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum Bucket {
    Today,
    Yesterday,
    ThisWeek,
    ThisMonth,
    Older,
    Examples,
}

impl Bucket {
    /// The section label shown for the bucket (`buildGroups` group names).
    pub fn label(self) -> &'static str {
        match self {
            Bucket::Today => "Today",
            Bucket::Yesterday => "Yesterday",
            Bucket::ThisWeek => "This week",
            Bucket::ThisMonth => "This month",
            Bucket::Older => "Older",
            Bucket::Examples => "Examples",
        }
    }

    /// Display order, smallest first.
    fn order(self) -> u8 {
        match self {
            Bucket::Today => 0,
            Bucket::Yesterday => 1,
            Bucket::ThisWeek => 2,
            Bucket::ThisMonth => 3,
            Bucket::Older => 4,
            Bucket::Examples => 5,
        }
    }
}

/// Pick the date bucket for a recent entry (`buildGroups` bucketing rule,
/// `startpage.h:236-261`): examples → `Examples`; else by `age_days`:
/// 0 → Today, 1 → Yesterday, <7 → ThisWeek, <31 → ThisMonth, else Older.
///
/// (The C++ used calendar "same month + year" for ThisMonth; we approximate with
/// a <31-day window — the precise calendar test is the same intent and is kept
/// deterministic here without a clock.)
pub fn bucket_for(entry: &RecentEntry) -> Bucket {
    if entry.is_example {
        return Bucket::Examples;
    }
    match entry.age_days {
        d if d <= 0 => Bucket::Today,
        1 => Bucket::Yesterday,
        d if d < 7 => Bucket::ThisWeek,
        d if d < 31 => Bucket::ThisMonth,
        _ => Bucket::Older,
    }
}

/// A built group: a bucket + its entries (`struct Group`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Group {
    pub bucket: Bucket,
    pub entries: Vec<RecentEntry>,
}

/// Build the date-bucketed groups from recent entries, applying the search
/// `filter` (`buildGroups`): an entry matches when `filter` (lowercased) is a
/// substring of its file name or dir path; empty filter matches all. Only
/// non-empty buckets are emitted, in [`Bucket`] order; within a bucket the
/// original order is preserved.
pub fn build_groups(entries: &[RecentEntry], filter: &str) -> Vec<Group> {
    let f = filter.trim().to_lowercase();
    let matches = |e: &RecentEntry| -> bool {
        if f.is_empty() {
            return true;
        }
        e.file_name.to_lowercase().contains(&f) || e.dir_path.to_lowercase().contains(&f)
    };

    // Bucket → entries, preserving input order within each bucket.
    let mut buckets: Vec<(Bucket, Vec<RecentEntry>)> = Vec::new();
    for e in entries.iter().filter(|e| matches(e)) {
        let b = bucket_for(e);
        if let Some(slot) = buckets.iter_mut().find(|(bb, _)| *bb == b) {
            slot.1.push(e.clone());
        } else {
            buckets.push((b, vec![e.clone()]));
        }
    }

    buckets.sort_by_key(|(b, _)| b.order());
    buckets
        .into_iter()
        .map(|(bucket, entries)| Group { bucket, entries })
        .collect()
}

/// The events the start page raises (the C++ signals, `startpage.h`).
#[derive(Clone, Debug)]
pub enum StartPageEvent {
    /// An action card was clicked (`newClass`/`openProject`/`importSource`/…).
    Card(StartCard),
    /// A recent-files entry was clicked (`fileSelected(path)`).
    FileSelected(String),
    /// ESC or outside-click — no specific action (`dismissed`).
    Dismissed,
}

// Start-page key action — dismiss the overlay on Escape (the C++
// `keyPressEvent(Escape)` → `dismissed`).
actions!(rcx_start_page, [StartPageDismiss]);

/// The key binding for the start page (Escape → dismiss).
pub fn start_page_key_bindings() -> Vec<KeyBinding> {
    vec![KeyBinding::new(
        "escape",
        StartPageDismiss,
        Some("RcxStartPage"),
    )]
}

/// The start-page overlay view.
///
/// Renders the title, the action-card column, and the date-bucketed recent list
/// with a search filter, raising [`StartPageEvent`]s. Shown over the workspace as
/// a full-window overlay (the window mounts/dismisses it; app-shell §13).
pub struct StartPage {
    entries: Vec<RecentEntry>,
    search: Entity<InputState>,
    focus_handle: FocusHandle,
}

impl StartPage {
    /// Build the start page over the given recent entries (the window supplies
    /// them from settings + the examples dir; `loadEntries`).
    pub fn new(entries: Vec<RecentEntry>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search recent..."));
        StartPage {
            entries,
            search,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Construct as an [`Entity`].
    pub fn view(entries: Vec<RecentEntry>, window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| StartPage::new(entries, window, cx))
    }

    /// The current search filter text.
    fn filter(&self, cx: &App) -> String {
        self.search.read(cx).value().to_string()
    }

    fn on_dismiss(&mut self, _: &StartPageDismiss, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(StartPageEvent::Dismissed);
    }

    /// The "Tutorial →" link was clicked (the C++ `continueClicked` →
    /// dismiss + open the tutorial/help flow; `main.cpp:9415`). We open the
    /// in-app command palette — the window's help/docs entry point (the
    /// `help.docs` notify text directs users there) — by dispatching the
    /// window's [`OpenCommandPalette`](super::window::OpenCommandPalette) action.
    /// The start-page overlay sits inside the window's `RcxWindow` dispatch
    /// subtree, so the action bubbles up to the window's `on_action` handler.
    fn on_tutorial(&mut self, _e: &gpui::ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.dispatch_action(Box::new(super::window::OpenCommandPalette), cx);
    }

    /// Render one action row (`drawCards`) as a Zed welcome-list row: a tinted
    /// leading icon tile, a bold title over a muted subtitle, and a right-aligned
    /// keybinding hint where one applies. The whole row is the click target, with
    /// a hover-overlay lift and a soft-accent rounded surface (no hard fill, no
    /// loud left bar — content leads, chrome recedes).
    fn render_card(&self, card: StartCard, cx: &mut Context<Self>) -> impl IntoElement {
        let accent = color::accent(cx);
        // The icon tile: a low-alpha tint of the accent behind the glyph — Zed's
        // restrained "content-forward" badge, not a saturated fill.
        let mut tile_bg = accent;
        tile_bg.a = 0.12;

        div()
            .id(card.id())
            .w_full()
            .cursor_pointer()
            .rounded(px(tokens::radius::LG))
            .hover(|s| s.bg(color::hover_overlay(cx)))
            .on_click(cx.listener(move |_this, _e, _window, cx| {
                cx.emit(StartPageEvent::Card(card));
            }))
            .child(
                gpui_component::h_flex()
                    .w_full()
                    .gap(px(tokens::space::LG))
                    .items_center()
                    .py(px(tokens::space::MD))
                    .px(px(tokens::space::MD))
                    .child(
                        // Leading icon tile.
                        div()
                            .flex_none()
                            .size(px(34.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(tokens::radius::MD))
                            .bg(tile_bg)
                            .text_color(accent)
                            .text_size(px(tokens::font::UI_LG))
                            .child(card.glyph()),
                    )
                    .child(
                        gpui_component::v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(tokens::space::XXS))
                            .child(
                                div()
                                    .text_size(px(tokens::font::UI_MD))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(color::text(cx))
                                    .child(card.title()),
                            )
                            .child(
                                div()
                                    .text_size(px(tokens::font::UI_SM))
                                    .text_color(color::text_muted(cx))
                                    .child(card.description()),
                            ),
                    )
                    .when_some(card.shortcut(), |row, keys| row.child(key_cap(keys, cx))),
            )
    }

    /// Render the recent-files list: the date-bucketed groups + a row per entry
    /// (`drawFileList`): file name + dim dir path; clicking a row selects the file.
    /// Section captions use [`design::section_label`]; each row is a
    /// [`design::zed_list_row`] with a leading glyph, the file name, and the
    /// middle-elided dir path muted alongside it.
    fn render_recent(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let groups = build_groups(&self.entries, &self.filter(cx));

        let mut col = gpui_component::v_flex().w_full().gap(px(tokens::space::XS));

        if groups.is_empty() {
            // The empty state — a quiet, centered hint (Zed's "no items yet" feel),
            // not a bare left-aligned line.
            col = col.child(
                gpui_component::v_flex()
                    .w_full()
                    .py(px(tokens::space::XXL))
                    .gap(px(tokens::space::XS))
                    .items_center()
                    .child(
                        div()
                            .text_size(px(tokens::font::UI_MD))
                            .text_color(color::text_muted(cx))
                            .child(if self.filter(cx).trim().is_empty() {
                                "No recent files"
                            } else {
                                "No matching files"
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(tokens::font::UI_SM))
                            .text_color(color::text_disabled(cx))
                            .child("Open a project to see it here"),
                    ),
            );
            return col;
        }

        for group in groups {
            // Section header (the collapsible group label; rendered static here —
            // the C++ expand/collapse toggle isn't part of the welcome restyle).
            col = col.child(crate::ui::design::section_label(group.bucket.label(), cx));
            for entry in group.entries {
                let path = entry.path.clone();
                let glyph = if entry.is_example {
                    "\u{1F4D6}" // 📖 example/book
                } else {
                    "\u{25A4}" // ▤ struct/class
                };
                col = col.child(
                    crate::ui::design::zed_list_row(
                        SharedString::from(format!("recent-{}", entry.path)),
                        false,
                        cx,
                    )
                    .h(px(30.))
                    .cursor_pointer()
                    .on_click(cx.listener(move |_this, _e, _window, cx| {
                        cx.emit(StartPageEvent::FileSelected(path.clone()));
                    }))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(tokens::font::UI_SM))
                            .text_color(color::text_muted(cx))
                            .child(glyph),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(tokens::font::UI_MD))
                            .text_color(color::text(cx))
                            .child(entry.file_name.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_size(px(tokens::font::UI_SM))
                            .text_color(color::text_muted(cx))
                            .child(entry.dir_path.clone()),
                    ),
                );
            }
        }
        col
    }
}

impl Focusable for StartPage {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<StartPageEvent> for StartPage {}

impl Render for StartPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let recent = self.render_recent(cx);
        let cards: Vec<_> = StartCard::ALL
            .iter()
            .map(|c| self.render_card(*c, cx).into_any_element())
            .collect();

        // The welcome surface: a full-window content-bg backdrop, with a single
        // horizontally-centered, comfortably-proportioned column (Zed's welcome
        // tab) — a brand header, the action list, then the recent-files block
        // under a divider.
        //
        // Centering is done in two layers so the column lands dead-center on a
        // wide window (and never hugs the left third): the backdrop is a flex
        // column that `items_center`s its child on the cross (horizontal) axis,
        // and the content column itself caps its width and `mx_auto`s — the
        // belt-and-braces Zed welcome-tab recipe so a stray grow can't bias it.
        div()
            .id("rcx-start-page")
            .track_focus(&self.focus_handle)
            .key_context("RcxStartPage")
            .on_action(cx.listener(Self::on_dismiss))
            .absolute()
            .inset_0()
            .size_full()
            .bg(color::content_bg(cx))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .p(px(tokens::space::XXL))
            .child(
                gpui_component::v_flex()
                    .flex_none()
                    .w(px(560.))
                    .max_w_full()
                    .mx_auto()
                    .gap(px(tokens::space::XXL))
                    // ── Brand header ──
                    .child(
                        gpui_component::v_flex()
                            .gap(px(tokens::space::XS))
                            .child(
                                div()
                                    .text_size(px(32.))
                                    .font_weight(FontWeight::LIGHT)
                                    .text_color(color::text(cx))
                                    .child("Reclass"),
                            )
                            .child(
                                div()
                                    .text_size(px(tokens::font::UI_MD))
                                    .text_color(color::text_muted(cx))
                                    .child("A faithful struct-layout editor"),
                            ),
                    )
                    // ── Get started ──
                    .child(
                        gpui_component::v_flex()
                            .gap(px(tokens::space::MD))
                            .child(crate::ui::design::section_label("Get started", cx))
                            .child(
                                gpui_component::v_flex()
                                    .gap(px(tokens::space::XXS))
                                    .children(cards),
                            )
                            .child(
                                // The "Tutorial →" link (the C++ centered tutorial
                                // link, `startpage.h:321` → `continueClicked` →
                                // dismiss + open the help/tutorial flow). Wired to
                                // the window's command palette (`OpenCommandPalette`,
                                // the in-app help/docs entry point the C++
                                // help.docs text points users to): dispatching the
                                // window action from the overlay bubbles up to the
                                // `RcxWindow` handler, which dismisses nothing here
                                // and opens the palette over the welcome page.
                                div()
                                    .id("start-tutorial")
                                    .cursor_pointer()
                                    .self_start()
                                    .px(px(tokens::space::MD))
                                    .pt(px(tokens::space::XS))
                                    .text_size(px(tokens::font::UI_SM))
                                    .text_color(color::link(cx))
                                    .hover(|s| s.underline())
                                    .on_click(cx.listener(Self::on_tutorial))
                                    .child("Tutorial \u{2192}"),
                            ),
                    )
                    // ── Open recent ──
                    .child(
                        gpui_component::v_flex()
                            .gap(px(tokens::space::MD))
                            .child(
                                gpui_component::h_flex()
                                    .w_full()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(tokens::space::MD))
                                    .child(crate::ui::design::section_label("Open recent", cx))
                                    .child(Input::new(&self.search).w(px(220.))),
                            )
                            .child(recent),
                    ),
            )
    }
}

/// A right-aligned key-cap chip (the keybinding hint on a welcome action row):
/// a small `SM`-radius outlined pill in `UI_XS` muted text — the shared spec's
/// key-cap recipe (`zed_ui_spec.md` §6 "Key-cap chip").
fn key_cap(keys: &str, cx: &gpui::App) -> impl IntoElement {
    div()
        .flex_none()
        .px(px(tokens::space::SM))
        .py(px(tokens::space::XXS))
        .rounded(px(tokens::radius::SM))
        .border_1()
        .border_color(color::border(cx))
        .text_size(px(tokens::font::UI_XS))
        .text_color(color::text_muted(cx))
        .child(SharedString::from(keys.to_string()))
}

#[cfg(test)]
mod tests {
    // Import only the gpui-free data items under test — NOT `super::*`, which
    // would pull the module's `gpui::*` glob into the `#[test]` hygiene
    // expansion and overflow the type-recursion budget (see lib.rs note).
    use super::{bucket_for, build_groups, Bucket, RecentEntry, StartCard};

    fn entry(name: &str, dir: &str, age: i64, example: bool) -> RecentEntry {
        RecentEntry {
            path: format!("{dir}/{name}"),
            file_name: name.to_string(),
            dir_path: dir.to_string(),
            age_days: age,
            is_example: example,
        }
    }

    #[test]
    fn cards_are_in_stable_hit_test_order() {
        // Card index → action parity (hitTest cards 0..4).
        assert_eq!(StartCard::ALL[0], StartCard::NewClass);
        assert_eq!(StartCard::ALL[1], StartCard::OpenProject);
        assert_eq!(StartCard::ALL[2], StartCard::ImportSource);
        assert_eq!(StartCard::ALL[3], StartCard::ImportXml);
        assert_eq!(StartCard::ALL[4], StartCard::ImportPdb);
        assert_eq!(StartCard::NewClass.title(), "New Class");
        assert!(!StartCard::OpenProject.description().is_empty());
    }

    #[test]
    fn bucketing_matches_day_deltas() {
        assert_eq!(bucket_for(&entry("a.rcx", "/p", 0, false)), Bucket::Today);
        assert_eq!(
            bucket_for(&entry("a.rcx", "/p", 1, false)),
            Bucket::Yesterday
        );
        assert_eq!(
            bucket_for(&entry("a.rcx", "/p", 3, false)),
            Bucket::ThisWeek
        );
        assert_eq!(
            bucket_for(&entry("a.rcx", "/p", 20, false)),
            Bucket::ThisMonth
        );
        assert_eq!(bucket_for(&entry("a.rcx", "/p", 90, false)), Bucket::Older);
        // Examples always bucket to Examples regardless of age.
        assert_eq!(
            bucket_for(&entry("demo.rcx", "/ex", 0, true)),
            Bucket::Examples
        );
    }

    #[test]
    fn groups_built_in_bucket_order_and_skip_empty() {
        let entries = vec![
            entry("old.rcx", "/a", 90, false),  // Older
            entry("today.rcx", "/b", 0, false), // Today
            entry("week.rcx", "/c", 3, false),  // ThisWeek
            entry("demo.rcx", "/ex", 5, true),  // Examples
        ];
        let groups = build_groups(&entries, "");
        let order: Vec<Bucket> = groups.iter().map(|g| g.bucket).collect();
        // Today < ThisWeek < Older < Examples (Yesterday/ThisMonth empty → skipped).
        assert_eq!(
            order,
            vec![
                Bucket::Today,
                Bucket::ThisWeek,
                Bucket::Older,
                Bucket::Examples
            ]
        );
    }

    #[test]
    fn filter_matches_name_or_dir_case_insensitively() {
        let entries = vec![
            entry("Player.rcx", "/game/structs", 0, false),
            entry("Enemy.rcx", "/game/ai", 0, false),
        ];
        // Match by file name.
        let g = build_groups(&entries, "play");
        let names: Vec<&str> = g
            .iter()
            .flat_map(|g| g.entries.iter())
            .map(|e| e.file_name.as_str())
            .collect();
        assert_eq!(names, vec!["Player.rcx"]);

        // Match by directory.
        let g = build_groups(&entries, "AI");
        let names: Vec<&str> = g
            .iter()
            .flat_map(|g| g.entries.iter())
            .map(|e| e.file_name.as_str())
            .collect();
        assert_eq!(names, vec!["Enemy.rcx"]);

        // Empty filter → everything.
        let g = build_groups(&entries, "  ");
        let total: usize = g.iter().map(|g| g.entries.len()).sum();
        assert_eq!(total, 2);

        // No match → no groups.
        assert!(build_groups(&entries, "zzz").is_empty());
    }

    #[test]
    fn entries_preserve_order_within_a_bucket() {
        let entries = vec![
            entry("b.rcx", "/p", 0, false),
            entry("a.rcx", "/p", 0, false),
            entry("c.rcx", "/p", 0, false),
        ];
        let g = build_groups(&entries, "");
        assert_eq!(g.len(), 1);
        let names: Vec<&str> = g[0].entries.iter().map(|e| e.file_name.as_str()).collect();
        // Input order preserved (no re-sort within a bucket).
        assert_eq!(names, vec!["b.rcx", "a.rcx", "c.rcx"]);
    }
}
