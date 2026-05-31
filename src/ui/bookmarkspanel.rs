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

// ── gpui view ───────────────────────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use view::{BookmarkAction, BookmarksPanel};

#[cfg(feature = "ui")]
mod view {
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::dock::{Panel, PanelEvent};

    use super::{build_bookmark_rows, BookmarkRow};
    use crate::core::Bookmark;
    use crate::ui::design::{color, icon, tokens};

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
        focus_handle: FocusHandle,
    }

    impl BookmarksPanel {
        /// Build an empty bookmarks panel.
        pub fn new(cx: &mut Context<Self>) -> Self {
            BookmarksPanel {
                rows: Vec::new(),
                focus_handle: cx.focus_handle(),
            }
        }

        /// Construct as an [`Entity`] (the form a dock holds). Takes `window` to
        /// match the other panels' `view(window, cx)` shape.
        pub fn view(_window: &mut Window, cx: &mut App) -> Entity<Self> {
            cx.new(BookmarksPanel::new)
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

    impl Render for BookmarksPanel {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let view = cx.entity();
            let rows = self.rows.clone();
            let is_empty = rows.is_empty();

            gpui_component::v_flex()
                .id("rcx-bookmarks-panel")
                .track_focus(&self.focus_handle)
                .size_full()
                .bg(color::panel_bg(cx))
                .text_color(color::text(cx))
                .child(render_header(&view, rows.len(), cx))
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

        gpui_component::h_flex()
            .id(("rcx-bookmark-row", index))
            .w_full()
            .h(px(36.0))
            .px(px(tokens::space::MD))
            .gap(px(tokens::space::MD))
            .items_center()
            .rounded(px(tokens::radius::MD))
            .hover(|s| s.bg(color::hover_overlay(cx)))
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
                            .font_family(tokens::font::MONO_FAMILY)
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
    }

    /// The clean empty-state body: a centered muted caption (Zed panel state).
    fn empty_state(cx: &App) -> impl IntoElement {
        gpui_component::v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(px(tokens::space::MD))
            .child(
                icon::pointer()
                    .with_size(px(20.0))
                    .text_color(color::text_disabled(cx)),
            )
            .child(
                div()
                    .text_size(px(tokens::font::UI_SM))
                    .text_color(color::text_muted(cx))
                    .child("No bookmarks — add one from an address"),
            )
    }

    use gpui_component::Sizable as _;
}

#[cfg(test)]
mod tests {
    use super::{build_bookmark_rows, BookmarkRow};
    use crate::core::Bookmark;

    fn bm(name: &str, formula: &str) -> Bookmark {
        Bookmark {
            name: name.to_string(),
            address_formula: formula.to_string(),
        }
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
