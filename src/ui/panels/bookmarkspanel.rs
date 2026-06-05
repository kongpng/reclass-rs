//! Bookmarks dock — saved addresses for the active document (the C++ View ▸
//! Bookmarks side panel, `Ctrl+Shift+B`).
//!
//! Port of the Reclass "Bookmarks" right-side panel: a list of user-saved
//! addresses (each a name + an address formula like `"<game.exe>+0x12340"` that
//! survives rebases), each row re-navigating the editor to that address when
//! activated, with add / remove affordances.
//!
//! The controller owns the model: `add_bookmark` / `remove_bookmark`
//! (`controller.rs:3073`/`3088`) mutate `doc.tree.bookmarks`, which is a **public
//! field** ([`NodeTree::bookmarks`](crate::core::NodeTree)) — so reading the list
//! needs **no new accessor** (the window passes the bookmark slice in via
//! [`set_bookmarks`](self::view::BookmarksPanel::set_bookmarks)). Mutations route
//! back up through [`BookmarkAction`] events the window resolves onto the
//! controller (the read-only-surface pattern: the panel raises intents, the
//! controller applies them).
//!
//! Two halves (gpui-free model + thin view):
//! - [`BookmarkRow`] — one bookmark's display strings (name + formula), built
//!   from a [`Bookmark`](crate::core::Bookmark); pure + unit-tested.
//! - [`build_bookmark_rows`] — map the document's bookmark slice into rows
//!   (preserving order; the index is the `remove_bookmark` key); pure + tested.
//! - [`BookmarksPanel`] (gated on `ui`) — the Zed-styled gpui-component
//!   [`Panel`](gpui_component::dock::Panel): a header with an "Add" action over
//!   the bookmark rows (each with a remove affordance), with a clean empty-state.
//!
//! Gated behind the `ui` feature.

use gpui::SharedString;

use crate::core::Bookmark;

/// The stable `panel_name` for layout (de)serialization
/// (`DockArea::dump`/`load`). Must stay stable once docks persist it.
pub const PANEL_NAME: &str = "BookmarksPanel";

/// The dock's display title (header / tab label).
pub fn title() -> SharedString {
    SharedString::from("Bookmarks")
}

/// One bookmark row's display strings + its stable index — the name, the address
/// formula, and the list index that the controller's `remove_bookmark(idx)`
/// keys on. Pure + unit-tested.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookmarkRow {
    /// The list index (the `remove_bookmark` key; the row's stable id).
    pub index: usize,
    /// The bookmark's display name.
    pub name: String,
    /// The address formula (e.g. `"<game.exe>+0x12340"`).
    pub formula: String,
}

impl BookmarkRow {
    /// Build a row from a [`Bookmark`] at list index `index`.
    pub fn from_bookmark(index: usize, b: &Bookmark) -> BookmarkRow {
        BookmarkRow {
            index,
            name: if b.name.is_empty() {
                "(unnamed)".to_string()
            } else {
                b.name.clone()
            },
            formula: b.address_formula.clone(),
        }
    }
}

/// Map the document's bookmark slice into display rows, preserving order (the
/// index is the `remove_bookmark` key). Pure + unit-tested.
pub fn build_bookmark_rows(bookmarks: &[Bookmark]) -> Vec<BookmarkRow> {
    bookmarks
        .iter()
        .enumerate()
        .map(|(i, b)| BookmarkRow::from_bookmark(i, b))
        .collect()
}

/// Filter bookmark rows by a case-insensitive substring of the name OR formula
/// (the C++ "Filter bookmarks…" line edit). An empty query keeps everything;
/// the row's stable `index` (the `remove_bookmark` key) is preserved so a
/// filtered view still removes the right bookmark. Pure + unit-tested.
pub fn filter_bookmark_rows<'a>(rows: &'a [BookmarkRow], query: &str) -> Vec<&'a BookmarkRow> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return rows.iter().collect();
    }
    rows.iter()
        .filter(|r| r.name.to_lowercase().contains(&q) || r.formula.to_lowercase().contains(&q))
        .collect()
}

// ── gpui view ───────────────────────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use view::{BookmarkAction, BookmarksPanel};

#[cfg(feature = "ui")]
mod view {
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::dock::{Panel, PanelEvent};
    use gpui_component::input::{Input, InputEvent, InputState};
    use gpui_component::menu::{ContextMenuExt as _, PopupMenu};

    use super::{build_bookmark_rows, filter_bookmark_rows, BookmarkRow};
    use crate::core::Bookmark;
    use crate::ui::design::{color, icon, tokens};

    // ── Row context-menu actions (the C++ bookmark row right-click) ──
    //
    // Dispatched by the `PopupMenu` a row builds; the panel records the
    // right-clicked row's index in `context_target` so the action handler knows
    // which bookmark to act on, then re-emits the matching `BookmarkAction`.
    gpui::actions!(rcx_bookmarks, [BmNavigate, BmRemove]);

    /// An intent raised by the bookmarks panel for the window to resolve onto the
    /// controller (the read-only-surface pattern — the panel does not own the
    /// model, so it emits requests). The window applies these via the
    /// controller's `add_bookmark` / `remove_bookmark` and the editor's go-to.
    #[derive(Clone, Debug)]
    pub enum BookmarkAction {
        /// "Go to" a bookmark — re-navigate the editor to its address formula
        /// (the C++ row-activation). Carries the formula so the window can
        /// resolve it without re-reading the list.
        Navigate { formula: String },
        /// Remove the bookmark at this list index (`remove_bookmark(idx)`).
        Remove { index: usize },
        /// Request the "Add Bookmark" flow (the header "+" — the window opens the
        /// name/formula prompt, then calls `add_bookmark`).
        Add,
    }

    /// The Bookmarks right-dock panel — an "Add" action over the document's saved
    /// addresses, each with a remove affordance.
    ///
    /// Holds only the current bookmark rows (pushed in by the window via
    /// [`set_bookmarks`](BookmarksPanel::set_bookmarks) on document change — the
    /// model lives on the controller). Mirrors [`WorkspacePanel`] /
    /// [`ScannerPanel`]'s `view(window, cx) -> Entity<Self>` + `impl Panel`
    /// structure; raises [`BookmarkAction`] for the window to resolve.
    pub struct BookmarksPanel {
        rows: Vec<BookmarkRow>,
        /// The substring filter input (the C++ "Filter bookmarks…" line edit).
        filter_input: Entity<InputState>,
        /// Current filter text (synced from the input on change).
        filter: String,
        /// The row index a right-click context menu targets (the C++
        /// right-clicked bookmark), set before the menu's action fires.
        context_target: Option<usize>,
        focus_handle: FocusHandle,
        _subs: Vec<Subscription>,
    }

    impl BookmarksPanel {
        /// Build an empty bookmarks panel.
        pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
            let filter_input =
                cx.new(|cx| InputState::new(window, cx).placeholder("Filter bookmarks..."));
            let mut subs = Vec::new();
            subs.push(
                cx.subscribe(&filter_input, |this, input, ev: &InputEvent, cx| {
                    if matches!(ev, InputEvent::Change) {
                        this.filter = input.read(cx).value().to_string();
                        cx.notify();
                    }
                }),
            );
            BookmarksPanel {
                rows: Vec::new(),
                filter_input,
                filter: String::new(),
                context_target: None,
                focus_handle: cx.focus_handle(),
                _subs: subs,
            }
        }

        /// Construct as an [`Entity`] (the form a dock holds). Takes `window` to
        /// match the other panels' `view(window, cx)` shape.
        pub fn view(window: &mut Window, cx: &mut App) -> Entity<Self> {
            cx.new(|cx| BookmarksPanel::new(window, cx))
        }

        /// Replace the displayed bookmark rows (the window calls this on document
        /// change, reading `controller.document().tree.bookmarks`). No new
        /// accessor is needed — `bookmarks` is a public field.
        pub fn set_bookmarks(&mut self, bookmarks: &[Bookmark], cx: &mut Context<Self>) {
            self.rows = build_bookmark_rows(bookmarks);
            cx.notify();
        }

        /// The current rows (for tests / external wiring).
        pub fn rows(&self) -> &[BookmarkRow] {
            &self.rows
        }
    }

    impl Panel for BookmarksPanel {
        fn panel_name(&self) -> &'static str {
            super::PANEL_NAME
        }

        fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            SharedString::from("Bookmarks")
        }
    }

    impl EventEmitter<PanelEvent> for BookmarksPanel {}
    impl EventEmitter<BookmarkAction> for BookmarksPanel {}

    impl Focusable for BookmarksPanel {
        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.focus_handle.clone()
        }
    }

    impl BookmarksPanel {
        /// Context-menu "Navigate" (the C++ row right-click → Navigate): re-emit
        /// `BookmarkAction::Navigate` for the right-clicked row.
        fn on_ctx_navigate(&mut self, _: &BmNavigate, _w: &mut Window, cx: &mut Context<Self>) {
            if let Some(ix) = self.context_target {
                if let Some(row) = self.rows.get(ix) {
                    let formula = row.formula.clone();
                    cx.emit(BookmarkAction::Navigate { formula });
                }
            }
        }

        /// Context-menu "Remove" (the C++ row right-click → Remove).
        fn on_ctx_remove(&mut self, _: &BmRemove, _w: &mut Window, cx: &mut Context<Self>) {
            if let Some(index) = self.context_target {
                cx.emit(BookmarkAction::Remove { index });
            }
        }
    }

    impl Render for BookmarksPanel {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let view = cx.entity();
            // Apply the substring filter (the C++ "Filter bookmarks…" line edit),
            // keeping each row's stable remove-index.
            let rows: Vec<BookmarkRow> = filter_bookmark_rows(&self.rows, &self.filter)
                .into_iter()
                .cloned()
                .collect();
            let is_empty = rows.is_empty();
            let has_bookmarks = !self.rows.is_empty();

            gpui_component::v_flex()
                .id("rcx-bookmarks-panel")
                .track_focus(&self.focus_handle)
                .size_full()
                .bg(color::panel_bg(cx))
                .text_color(color::text(cx))
                .on_action(cx.listener(Self::on_ctx_navigate))
                .on_action(cx.listener(Self::on_ctx_remove))
                .child(render_header(&view, self.rows.len(), cx))
                // Filter row (only when there are bookmarks to filter).
                .when(has_bookmarks, |col| {
                    col.child(
                        gpui_component::h_flex()
                            .px(px(tokens::space::LG))
                            .py(px(tokens::space::XS))
                            .child(Input::new(&self.filter_input).small().flex_1()),
                    )
                })
                .child(div().flex_1().min_h_0().child(if is_empty {
                    empty_state(cx).into_any_element()
                } else {
                    bookmark_list(&view, rows, cx).into_any_element()
                }))
        }
    }

    /// The Zed panel header: an uppercase muted "BOOKMARKS" title + count on the
    /// left, an "Add" ("+") action on the right (the C++ bookmarks dock header).
    fn render_header(view: &Entity<BookmarksPanel>, count: usize, cx: &App) -> impl IntoElement {
        let add_view = view.clone();
        gpui_component::h_flex()
            .h(px(32.0))
            .w_full()
            .flex_none()
            .px(px(tokens::space::LG))
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(color::border(cx))
            .child(
                gpui_component::h_flex()
                    .gap(px(tokens::space::MD))
                    .items_baseline()
                    .child(
                        div()
                            .text_size(px(tokens::font::UI_SM))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(color::text_muted(cx))
                            .child("BOOKMARKS"),
                    )
                    .when(count > 0, |this| {
                        this.child(
                            div()
                                .text_size(px(tokens::font::UI_XS))
                                .text_color(color::text_disabled(cx))
                                .child(count.to_string()),
                        )
                    }),
            )
            // Add affordance — raises `BookmarkAction::Add` for the window to
            // open the name/formula prompt then call `add_bookmark`.
            .child(
                gpui_component::h_flex()
                    .id("rcx-bookmarks-add")
                    .flex_none()
                    .h(px(20.0))
                    .px(px(tokens::space::SM))
                    .gap(px(tokens::space::XS))
                    .items_center()
                    .rounded(px(tokens::radius::SM))
                    .text_size(px(tokens::font::UI_SM))
                    .text_color(color::text_muted(cx))
                    .hover(|s| s.bg(color::hover_overlay(cx)).text_color(color::text(cx)))
                    .child(icon::plus().with_size(px(12.0)))
                    .child("Add")
                    .on_click(move |_e, _w, cx| {
                        add_view.update(cx, |_this, cx| cx.emit(BookmarkAction::Add));
                    }),
            )
    }

    /// The bookmark list — one row per saved address (name + formula + remove).
    /// Not virtualized: bookmark counts are small.
    fn bookmark_list(
        view: &Entity<BookmarksPanel>,
        rows: Vec<BookmarkRow>,
        cx: &App,
    ) -> impl IntoElement {
        gpui_component::v_flex()
            .id("rcx-bookmarks-list")
            .size_full()
            .px(px(tokens::space::SM))
            .py(px(tokens::space::XS))
            .overflow_y_scroll()
            .children(rows.into_iter().map(|row| bookmark_row(view, row, cx)))
    }

    /// One bookmark row: a leading bookmark glyph, the name over its muted
    /// monospace formula, and a trailing remove ("×") affordance. Clicking the
    /// row navigates; clicking the × removes (both routed up via
    /// [`BookmarkAction`]).
    fn bookmark_row(view: &Entity<BookmarksPanel>, row: BookmarkRow, cx: &App) -> impl IntoElement {
        let index = row.index;
        let nav_view = view.clone();
        let nav_formula = row.formula.clone();
        let rm_view = view.clone();
        let ctx_view = view.clone();

        gpui_component::h_flex()
            .id(("rcx-bookmark-row", index))
            .w_full()
            .h(px(36.0))
            .px(px(tokens::space::MD))
            .gap(px(tokens::space::MD))
            .items_center()
            .rounded(px(tokens::radius::MD))
            .hover(|s| s.bg(color::hover_overlay(cx)))
            // Record the right-clicked row before the context menu's action
            // fires, so Navigate/Remove know which bookmark they target.
            .on_mouse_down(MouseButton::Right, move |_e, _w, cx| {
                ctx_view.update(cx, |this, _cx| this.context_target = Some(index));
            })
            .child(
                icon::pointer()
                    .with_size(px(12.0))
                    .text_color(color::accent(cx)),
            )
            // Name over the muted formula.
            .child(
                gpui_component::v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(tokens::space::XXS))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(tokens::font::UI_MD))
                            .text_color(color::text(cx))
                            .child(SharedString::from(row.name)),
                    )
                    .child(
                        div()
                            .truncate()
                            .font_family(tokens::font::mono_family())
                            .text_size(px(tokens::font::UI_XS))
                            .text_color(color::text_muted(cx))
                            .child(SharedString::from(row.formula)),
                    ),
            )
            // Navigate on row click.
            .on_click(move |_e, _w, cx| {
                let formula = nav_formula.clone();
                nav_view.update(cx, |_this, cx| {
                    cx.emit(BookmarkAction::Navigate { formula });
                });
            })
            // Remove affordance.
            .child(
                div()
                    .id(("rcx-bookmark-remove", index))
                    .flex_none()
                    .size(px(18.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(tokens::radius::SM))
                    .text_color(color::text_muted(cx))
                    .hover(|s| s.bg(color::hover_overlay(cx)).text_color(color::text(cx)))
                    .child(icon::close().with_size(px(11.0)))
                    .on_click(move |_e, _w, cx| {
                        // Stop the click from also navigating (the row's handler).
                        cx.stop_propagation();
                        rm_view.update(cx, |_this, cx| {
                            cx.emit(BookmarkAction::Remove { index });
                        });
                    }),
            )
            // Right-click context menu: Navigate / Remove (the C++ row menu).
            // Attached last so the interactive builders above stay accessible.
            .context_menu(move |menu: PopupMenu, _window, _cx| {
                menu.menu("Navigate", Box::new(BmNavigate))
                    .separator()
                    .menu("Remove", Box::new(BmRemove))
            })
    }

    /// The clean empty-state body: a centered muted caption (Zed panel state).
    fn empty_state(cx: &App) -> impl IntoElement {
        crate::ui::design::empty_state(icon::pointer(), "No bookmarks — add one from an address", cx)
    }

    use gpui_component::Sizable as _;
}

#[cfg(test)]
mod tests {
    use super::{build_bookmark_rows, filter_bookmark_rows, BookmarkRow};
    use crate::core::Bookmark;

    fn bm(name: &str, formula: &str) -> Bookmark {
        Bookmark {
            name: name.to_string(),
            address_formula: formula.to_string(),
        }
    }

    #[test]
    fn filter_matches_name_or_formula_and_keeps_index() {
        let bms = vec![
            bm("Player base", "<game.exe>+0x100"),
            bm("Health", "<game.exe>+0x200"),
            bm("Ammo", "0xDEAD"),
        ];
        let rows = build_bookmark_rows(&bms);
        // Match by name substring (case-insensitive).
        let by_name = filter_bookmark_rows(&rows, "health");
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].name, "Health");
        // The preserved index is the remove key (position 1).
        assert_eq!(by_name[0].index, 1);
        // Match by formula substring.
        let by_formula = filter_bookmark_rows(&rows, "dead");
        assert_eq!(by_formula.len(), 1);
        assert_eq!(by_formula[0].index, 2);
        // "game.exe" matches the two with that module formula.
        assert_eq!(filter_bookmark_rows(&rows, "game.exe").len(), 2);
        // Empty query keeps all.
        assert_eq!(filter_bookmark_rows(&rows, "   ").len(), 3);
        // No match.
        assert!(filter_bookmark_rows(&rows, "zzz").is_empty());
    }

    #[test]
    fn bookmark_row_carries_index_name_and_formula() {
        let b = bm("Player base", "<game.exe>+0x12340");
        let row = BookmarkRow::from_bookmark(2, &b);
        assert_eq!(row.index, 2);
        assert_eq!(row.name, "Player base");
        assert_eq!(row.formula, "<game.exe>+0x12340");
    }

    #[test]
    fn unnamed_bookmark_gets_placeholder_name() {
        let b = bm("", "0x400000");
        let row = BookmarkRow::from_bookmark(0, &b);
        assert_eq!(row.name, "(unnamed)");
        // The formula is preserved verbatim.
        assert_eq!(row.formula, "0x400000");
    }

    #[test]
    fn build_rows_preserves_order_and_indexes() {
        let bms = vec![bm("a", "0x1"), bm("b", "0x2"), bm("c", "0x3")];
        let rows = build_bookmark_rows(&bms);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
        // The index is the position in the list (the remove_bookmark key).
        let idxs: Vec<usize> = rows.iter().map(|r| r.index).collect();
        assert_eq!(idxs, vec![0, 1, 2]);
    }

    #[test]
    fn build_rows_empty_for_no_bookmarks() {
        assert!(build_bookmark_rows(&[]).is_empty());
    }
}
