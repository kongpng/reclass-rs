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

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputState};
use gpui_component::ActiveTheme;

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

    /// Render one action card (`drawCards`): title + dim description, hover fill +
    /// a left accent bar (the C++ "3px accent left bar").
    fn render_card(&self, card: StartCard, cx: &mut Context<Self>) -> impl IntoElement {
        Button::new(card.id())
            .ghost()
            .w_full()
            .on_click(cx.listener(move |_this, _e, _window, cx| {
                cx.emit(StartPageEvent::Card(card));
            }))
            .child(
                gpui_component::h_flex()
                    .w_full()
                    .gap_3()
                    .items_center()
                    .py_2()
                    .px_3()
                    .border_l_2()
                    .border_color(cx.theme().accent)
                    .child(
                        gpui_component::v_flex()
                            .gap_0p5()
                            .child(
                                div()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(cx.theme().foreground)
                                    .child(card.title()),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(card.description()),
                            ),
                    ),
            )
    }

    /// Render the recent-files list: the date-bucketed groups + a row per entry
    /// (`drawFileList`): file name + dim dir path; clicking a row selects the file.
    fn render_recent(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let groups = build_groups(&self.entries, &self.filter(cx));

        let mut col = gpui_component::v_flex().w_full().gap_2();

        if groups.is_empty() {
            col = col.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("No recent files"),
            );
            return col;
        }

        for group in groups {
            // Section header (the collapsible group label; we render it static).
            col = col.child(
                div()
                    .pt_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(group.bucket.label().to_uppercase()),
            );
            for entry in group.entries {
                let path = entry.path.clone();
                col = col.child(
                    Button::new(SharedString::from(format!("recent-{}", entry.path)))
                        .ghost()
                        .w_full()
                        .on_click(cx.listener(move |_this, _e, _window, cx| {
                            cx.emit(StartPageEvent::FileSelected(path.clone()));
                        }))
                        .child(
                            gpui_component::h_flex()
                                .w_full()
                                .gap_2()
                                .items_baseline()
                                .child(
                                    div()
                                        .text_color(cx.theme().foreground)
                                        .child(entry.file_name.clone()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(entry.dir_path.clone()),
                                ),
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
        // Left column = title + recent list; right column = action cards (the C++
        // "Open recent" left, cards right).
        let recent = self.render_recent(cx);
        let cards: Vec<_> = StartCard::ALL
            .iter()
            .map(|c| self.render_card(*c, cx).into_any_element())
            .collect();

        div()
            .id("rcx-start-page")
            .track_focus(&self.focus_handle)
            .key_context("RcxStartPage")
            .on_action(cx.listener(Self::on_dismiss))
            .absolute()
            .inset_0()
            .size_full()
            .bg(cx.theme().background)
            .flex()
            .flex_row()
            .p_8()
            .gap_8()
            .child(
                // Left: title + search + recent files.
                gpui_component::v_flex()
                    .flex_1()
                    .gap_4()
                    .min_w_0()
                    .child(
                        div()
                            .text_3xl()
                            .text_color(cx.theme().foreground)
                            .child("Reclass"),
                    )
                    .child(
                        div()
                            .text_lg()
                            .text_color(cx.theme().foreground)
                            .child("Open recent"),
                    )
                    .child(Input::new(&self.search).w(px(330.)))
                    .child(recent),
            )
            .child(
                // Right: the action cards.
                gpui_component::v_flex()
                    .w(px(340.))
                    .gap_2()
                    .child(
                        div()
                            .text_lg()
                            .text_color(cx.theme().foreground)
                            .child("Get started"),
                    )
                    .children(cards)
                    .child(
                        // The "Tutorial →" link (the C++ centered tutorial link).
                        Button::new("start-tutorial")
                            .ghost()
                            .label("Tutorial \u{2192}")
                            .when(false, |b| b), // link target wired with the help workflow
                    ),
            )
    }
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
