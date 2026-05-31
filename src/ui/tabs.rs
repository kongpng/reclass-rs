//! MDI document tabs — the center area that hosts one editor per open document,
//! with the always-visible tab strip, the "+" new-tab sentinel, per-tab source
//! icons, and the dual tree/rendered view-mode toggle.
//!
//! Port of the C++ document-tab / MDI system (app-shell §8). In Qt each open
//! struct view was a tabified `QDockWidget` with a sentinel `​` dock keeping the
//! `QTabBar` visible and supplying the "+" affordance. The cookbook (app-shell §8
//! "Rust/GPUI sentinel note") says to **render the tab strip directly** instead:
//! a persistent "+" tab + the documents as a `Vec`. That is what [`DocumentArea`]
//! does — it is the [`DockArea`](gpui_component::dock::DockArea) center
//! [`Panel`](gpui_component::dock::Panel), rendering a [`TabBar`] of
//! [`DocEntry`]s above the active document's [`RcxEditor`].
//!
//! Behaviors reproduced (app-shell §8 "replicate the behaviors"):
//! - **always-visible strip** + a trailing **"+" tab** that opens a new document,
//! - **per-tab source icon** (full opacity = live, dimmed = disconnected),
//! - **active-tab follows selection / close** (the `m_activeDocDock` rules),
//! - **never leave a blank area** — closing the last tab opens a fresh document,
//! - the **dual view-mode toggle** (tree ⇄ rendered C/C++) in the strip suffix.
//!
//! The drag-to-reorder/redock overlay, middle-click close, and right-click tab
//! context menu (app-shell §8/§9) are layered on by later workflows; this stage
//! establishes the strip + sentinel + source-icon + view-toggle chrome and wires
//! it to the editor surface.
//!
//! **Styling (Zed tab bar; `_design/zed_ui_spec.md` §5.6).** The strip is drawn
//! directly as a flat `chrome_bg` bar with a 1px bottom `border`. Each tab is a
//! bespoke row — a real-SVG **source icon** (`design::icon`, full opacity = live,
//! dimmed = disconnected), a middle-elided title, and a trailing slot that holds
//! the **modified dot** at rest and a reveal-on-hover **close ✕** (an
//! [`IconName::Close`] SVG). The **active** tab lifts to `tab_active`
//! (backgroundAlt) with full-contrast text and carries a 2px `accent` top edge;
//! inactive tabs are `text_muted` and lighten by a hover overlay. A trailing
//! SVG **"+"** affordance opens a new document. The dual tree/rendered view-mode
//! toggle is a Zed **segmented control** anchored at the bottom of the body
//! ("Reclass" | "Code", each with its own glyph) — the selected segment lifts out
//! of a recessed track, as in the C++ bottom view tabs.
//!
//! In **rendered** mode the body shows the real generated C/C++ (the
//! [`render_cpp_tree`] codegen) as a scrollable, read-only Zed code editor with a
//! muted line-number gutter and One Dark syntax highlighting — reclass PIC3's
//! right pane (see [`DocumentArea::render_code_view`]).
//!
//! Gated behind the `ui` feature.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::dock::{Panel, PanelControl, PanelEvent, TitleStyle};
use gpui_component::{Icon, IconName, Sizable as _};

use super::design::{color, icon, tokens};
use super::editor::RcxEditor;
use super::state::{DataSource, DocId, SourceKind, ViewMode};
use crate::generator::render_cpp_tree;

/// Tab-strip height (logical px). The C++ dock tab bar was a fixed 37px
/// (`MenuBarStyle::sizeFromContents` `CT_TabBarTab`, app-shell §3); Zed runs a
/// touch shorter for a tighter chrome.
const TAB_STRIP_H: f32 = 36.0;
/// Maximum tab width before the title middle-elides (Zed caps tab width so a long
/// struct name never crowds the strip).
const TAB_MAX_W: f32 = 220.0;
/// Bottom view-mode toggle bar height.
const VIEW_TOGGLE_H: f32 = 30.0;
/// One segment's height inside the view-mode toggle.
const SEGMENT_H: f32 = 22.0;

/// One open document tab in the center area — the per-tab UI state + its editor.
///
/// Mirrors the C++ `TabState` (the bits the center needs; app-shell §6): the
/// stable [`DocId`], the title, the active data source (the tab source-icon), the
/// per-pane view mode, and the owned [`RcxEditor`] view (one editor per tab — the
/// "one document per tab" model).
pub struct DocEntry {
    pub id: DocId,
    pub title: SharedString,
    pub source: DataSource,
    pub view_mode: ViewMode,
    /// Unsaved-changes flag — drives the tab's **modified dot** (Zed shows a dot
    /// in the close slot until the tab is hovered). Pure tab-strip chrome state
    /// (the authoritative dirty bit lives on the document/undo stack); the window
    /// pushes it in via [`DocumentArea::set_modified`].
    pub modified: bool,
    pub editor: Entity<RcxEditor>,
}

impl DocEntry {
    fn new(id: DocId, title: impl Into<SharedString>, editor: Entity<RcxEditor>) -> Self {
        DocEntry {
            id,
            title: title.into(),
            source: DataSource::none(),
            view_mode: ViewMode::default(),
            modified: false,
            editor,
        }
    }
}

/// An event the document area raises to the window (the C++ signal wiring,
/// app-shell §8 step 9). The window reflects these into [`AppState`](super::state)
/// and the workspace title.
#[derive(Clone, Debug)]
pub enum DocAreaEvent {
    /// A tab became active (`visibilityChanged` → `m_activeDocDock`).
    Activated(DocId),
    /// The "+" sentinel was clicked — open a fresh document (`project_new`).
    NewDocumentRequested,
    /// A tab was closed (`dock.destroyed`).
    Closed(DocId),
    /// The active tab's view mode changed (the dual toggle; `setViewMode`).
    ViewModeChanged(DocId, ViewMode),
}

/// The center MDI document area: a [`TabBar`] + the active editor.
///
/// A gpui-component [`Panel`] (the `DockArea` center). Owns the ordered tab list
/// and the active index, and emits [`DocAreaEvent`]s the window observes.
pub struct DocumentArea {
    tabs: Vec<DocEntry>,
    active: usize,
    focus_handle: FocusHandle,
    /// Monotonic id allocator — independent of the window's [`AppState`] so the
    /// area is self-contained, but kept in lockstep by the window's wiring.
    next_id: u64,
}

impl DocumentArea {
    /// Build the area with one initial document (the C++ "never leave a blank
    /// window": a document is always present; app-shell §8 step 9).
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut area = DocumentArea {
            tabs: Vec::new(),
            active: 0,
            focus_handle: cx.focus_handle(),
            next_id: 0,
        };
        area.push_document("Untitled", window, cx);
        area
    }

    /// Construct as an [`Entity`] (the form the dock holds).
    pub fn view(window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| DocumentArea::new(window, cx))
    }

    /// Allocate the next document id (monotonic, never reused).
    fn alloc_id(&mut self) -> DocId {
        self.next_id += 1;
        DocId::from_raw(self.next_id)
    }

    /// Append a new document tab hosting a fresh editor, and make it active
    /// (`createTab` + `m_activeDocDock = dock`). Returns its id.
    pub fn push_document(
        &mut self,
        title: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> DocId {
        let id = self.alloc_id();
        let editor = RcxEditor::view(window, cx);
        self.tabs.push(DocEntry::new(id, title, editor));
        self.active = self.tabs.len() - 1;
        id
    }

    /// The active document entry, if any.
    pub fn active_entry(&self) -> Option<&DocEntry> {
        self.tabs.get(self.active)
    }

    /// The active document's editor (for the window to push documents/options into).
    pub fn active_editor(&self) -> Option<&Entity<RcxEditor>> {
        self.active_entry().map(|e| &e.editor)
    }

    /// All open tabs, in strip order.
    pub fn tabs(&self) -> &[DocEntry] {
        &self.tabs
    }

    /// The active tab index.
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// Index of a tab by id.
    fn index_of(&self, id: DocId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// Set a tab's source (the tab source-icon + liveness; `refreshDocTabSourceIcon`).
    pub fn set_source(&mut self, id: DocId, source: DataSource, cx: &mut Context<Self>) {
        if let Some(i) = self.index_of(id) {
            self.tabs[i].source = source;
            cx.notify();
        }
    }

    /// Rename a tab (the `rootName(tree)` change → tab title).
    pub fn set_title(&mut self, id: DocId, title: impl Into<SharedString>, cx: &mut Context<Self>) {
        if let Some(i) = self.index_of(id) {
            self.tabs[i].title = title.into();
            cx.notify();
        }
    }

    /// Set a tab's modified (unsaved-changes) flag — the Zed modified dot
    /// (`undoStack.indexChanged` → "document is dirty"; app-shell §8 step 9).
    pub fn set_modified(&mut self, id: DocId, modified: bool, cx: &mut Context<Self>) {
        if let Some(i) = self.index_of(id) {
            if self.tabs[i].modified != modified {
                self.tabs[i].modified = modified;
                cx.notify();
            }
        }
    }

    /// Activate a tab by index (a tab-strip click). Emits [`DocAreaEvent::Activated`].
    fn activate_index(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.tabs.len() && ix != self.active {
            self.active = ix;
            let id = self.tabs[ix].id;
            cx.emit(DocAreaEvent::Activated(id));
            cx.notify();
        }
    }

    /// The "+" sentinel was clicked: open a fresh document and signal the window
    /// (`project_new`; app-shell §8 sentinel "+" click).
    fn on_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Default new-document title mirrors the C++ "UnnamedClassN" scheme at the
        // shell level; the real root name follows once a document is attached.
        self.push_document("Untitled", window, cx);
        cx.emit(DocAreaEvent::NewDocumentRequested);
        cx.notify();
    }

    /// Close the tab at `ix`, fixing up the active index (`dock.destroyed`:
    /// reassign `m_activeDocDock` to the last remaining tab). When the final tab
    /// closes, a fresh one is opened so the area is never blank (the C++
    /// "never leave a blank window" reflex; app-shell §8/§20).
    pub fn close_index(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let was_active = ix == self.active;
        let closed = self.tabs.remove(ix);
        cx.emit(DocAreaEvent::Closed(closed.id));

        if self.tabs.is_empty() {
            // Never leave a blank area.
            self.push_document("Untitled", window, cx);
            cx.emit(DocAreaEvent::NewDocumentRequested);
        } else {
            // Fix up the active index so the *same* document stays active:
            // - the active tab itself was closed → reassign to the last tab
            //   (the C++ "reassign `m_activeDocDock` to last" rule);
            // - a tab *before* the active one was removed → the active element
            //   shifted left by one, so decrement to follow it;
            // - a tab *after* the active one was removed → the active index is
            //   unchanged.
            if was_active {
                self.active = self.tabs.len() - 1;
            } else if ix < self.active {
                self.active -= 1;
            }
            self.active = self.active.min(self.tabs.len() - 1);
            let id = self.tabs[self.active].id;
            cx.emit(DocAreaEvent::Activated(id));
        }
        cx.notify();
    }

    /// Toggle the active tab's view mode (the dual tree/rendered toggle). Emits
    /// [`DocAreaEvent::ViewModeChanged`].
    fn toggle_view_mode(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.tabs.get_mut(self.active) {
            entry.view_mode = entry.view_mode.toggled();
            let (id, mode) = (entry.id, entry.view_mode);
            cx.emit(DocAreaEvent::ViewModeChanged(id, mode));
            cx.notify();
        }
    }

    /// Set the active tab's view mode explicitly (`setViewMode` from the window).
    pub fn set_active_view_mode(&mut self, mode: ViewMode, cx: &mut Context<Self>) {
        if let Some(entry) = self.tabs.get_mut(self.active) {
            if entry.view_mode != mode {
                entry.view_mode = mode;
                cx.notify();
            }
        }
    }

    /// Build the Zed tab bar: a flat `chrome_bg` strip with a 1px bottom border,
    /// one bespoke tab per document, then a trailing "+" new-document affordance.
    ///
    /// Each tab carries a source icon, a middle-elided title, an active 2px accent
    /// top edge + `tab_active` lift, and a trailing slot that holds the modified
    /// dot at rest / a reveal-on-hover close ✕ (spec §5.6).
    fn render_tab_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.active;

        gpui_component::h_flex()
            .id("rcx-doc-tabs")
            .w_full()
            .flex_none()
            .h(px(TAB_STRIP_H))
            .items_stretch()
            .bg(color::chrome_bg(cx))
            .border_b_1()
            .border_color(color::border(cx))
            // Scroll the tabs horizontally when they overflow; the "+" stays put.
            .child(
                gpui_component::h_flex()
                    .id("rcx-doc-tabs-scroll")
                    .flex_1()
                    .min_w_0()
                    .items_stretch()
                    .overflow_x_scroll()
                    .children(
                        self.tabs
                            .iter()
                            .enumerate()
                            .map(|(ix, entry)| self.render_tab(ix, entry, ix == active, cx)),
                    ),
            )
            // The trailing "+" new-document affordance (app-shell §8 "+" sentinel).
            .child(self.render_new_tab_button(cx))
    }

    /// One bespoke Zed document tab.
    fn render_tab(
        &self,
        ix: usize,
        entry: &DocEntry,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = entry.id;
        let group_name = SharedString::from(format!("rcx-tab-{}", id.get()));
        // Per-tab source icon — a real SVG (full opacity = live, dimmed = off).
        let source_badge = source_icon(entry.source.kind, entry.source.live, cx);

        // Trailing slot: the modified dot at rest, the close ✕ on hover. Both
        // occupy the same fixed-width slot so the title never shifts.
        let dot = div()
            .absolute()
            .size(px(6.0))
            .rounded(px(tokens::radius::FULL))
            .bg(if selected {
                color::text(cx)
            } else {
                color::text_muted(cx)
            })
            .when(!entry.modified, |d| d.invisible())
            .group_hover(group_name.clone(), |d| d.invisible());

        let close = div()
            .id(SharedString::from(format!("rcx-tab-close-{}", id.get())))
            .absolute()
            .flex()
            .items_center()
            .justify_center()
            .size(px(16.0))
            .rounded(px(tokens::radius::SM))
            .text_color(color::text_muted(cx))
            .invisible()
            .group_hover(group_name.clone(), |d| d.visible())
            .hover(|d| d.bg(color::hover_overlay(cx)).text_color(color::text(cx)))
            .child(icon::close().xsmall())
            // Swallow the press so closing a tab doesn't also activate it (the
            // close click must not bubble to the tab's `on_click`).
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _e, window, cx| {
                if let Some(i) = this.index_of(id) {
                    this.close_index(i, window, cx);
                }
            }));

        let trailing = div()
            .flex_none()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .size(px(16.0))
            .child(dot)
            .child(close);

        gpui_component::h_flex()
            .id(SharedString::from(format!("rcx-tab-{}", id.get())))
            .group(group_name)
            .relative()
            .flex_none()
            .h_full()
            .max_w(px(TAB_MAX_W))
            .items_center()
            .gap(px(tokens::space::SM))
            .px(px(tokens::space::LG))
            .border_r_1()
            .border_color(color::border(cx))
            .text_size(px(tokens::font::UI_SM))
            // Selected: lift to the elevated tab bg + full-contrast text so the
            // active tab reads as a connected surface (Zed active-tab); inactive:
            // muted text that lightens by a hover overlay only (no border change —
            // spec §5.6/§7), so the strip stays calm until pointed at.
            .map(|t| {
                if selected {
                    t.bg(color::elevated_bg(cx))
                        .text_color(color::text(cx))
                        .font_weight(FontWeight::MEDIUM)
                } else {
                    t.text_color(color::text_muted(cx))
                        .hover(|s| s.bg(color::hover_overlay(cx)).text_color(color::text(cx)))
                }
            })
            // The 2px accent top edge marks the active tab (spec §5.6; the C++
            // active-tab accent rail). Drawn as an overlaid bar so it sits flush
            // on the tab's top regardless of padding.
            .when(selected, |t| {
                t.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(tokens::border::THICK))
                        .bg(color::accent(cx)),
                )
            })
            .child(source_badge)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .whitespace_nowrap()
                    .child(entry.title.clone()),
            )
            .child(trailing)
            .on_click(cx.listener(move |this, _e, _window, cx| {
                this.activate_index(ix, cx);
            }))
    }

    /// The trailing "+" affordance — a ghost icon button (real SVG) that opens a
    /// new document (the C++ sentinel "+" click; app-shell §8).
    fn render_new_tab_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("rcx-new-tab")
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .w(px(TAB_STRIP_H))
            .h_full()
            .text_color(color::text_muted(cx))
            .hover(|d| d.bg(color::hover_overlay(cx)).text_color(color::text(cx)))
            .child(icon::plus().small())
            .on_click(cx.listener(|this, _e, window, cx| {
                this.on_new_tab(window, cx);
            }))
    }

    /// The dual view-mode toggle, styled as a Zed **segmented control** —
    /// "Reclass" (tree) | "Code" (rendered C/C++), each with a real SVG glyph.
    /// Matches the C++ bottom view tabs (PIC5/PIC2; `reclass_view_click_active`):
    /// the track is a recessed pill and the **selected segment lifts** out of it
    /// to the elevated surface for a clear, high-contrast active state.
    fn render_view_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view_mode = self.active_entry().map(|e| e.view_mode).unwrap_or_default();
        let has_doc = !self.tabs.is_empty();

        // One segment of the control. Every segment ALWAYS renders its glyph +
        // label (a generous min width + `flex_none` so the text never collapses).
        // The selected segment LIFTS out of the recessed track to the elevated
        // surface (a 1px border + soft shadow + full-contrast text), the way a Zed
        // segmented control reads; inactive segments are `text_muted` over the
        // track and brighten with a hover overlay. The active segment ignores
        // clicks (it is already shown); only the inactive one toggles.
        let segment =
            |label: &'static str, glyph: Icon, this_mode: ViewMode, cx: &mut Context<Self>| {
                let selected = view_mode == this_mode;
                gpui_component::h_flex()
                    .id(label)
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .gap(px(tokens::space::XS))
                    .h(px(SEGMENT_H))
                    .min_w(px(72.0))
                    .px(px(tokens::space::LG))
                    .rounded(px(tokens::radius::SM))
                    .text_size(px(tokens::font::UI_SM))
                    .child(glyph.xsmall())
                    .child(label)
                    .map(|s| {
                        if selected {
                            // Lift to the elevated surface — a bordered, shadowed
                            // pill with full-contrast text (the active segment).
                            s.bg(color::elevated_bg(cx))
                                .border_1()
                                .border_color(color::border(cx))
                                .shadow_sm()
                                .text_color(color::text(cx))
                                .font_weight(FontWeight::MEDIUM)
                        } else {
                            // Transparent over the track; brighten on hover.
                            s.text_color(color::text_muted(cx)).hover(|h| {
                                h.bg(color::hover_overlay(cx)).text_color(color::text(cx))
                            })
                        }
                    })
                    .when(has_doc && !selected, |s| {
                        s.on_click(cx.listener(|this, _e, _window, cx| {
                            this.toggle_view_mode(cx);
                        }))
                    })
            };

        // The strip carries an explicit bg + top border so it never reads as
        // overlapped by the scanner dock above; the segmented control is a
        // recessed track (`chrome_bg`, 1px border) the active segment lifts out of.
        gpui_component::h_flex()
            .id("rcx-view-toggle")
            .flex_none()
            .h(px(VIEW_TOGGLE_H))
            .w_full()
            .items_center()
            .px(px(tokens::space::LG))
            .bg(color::chrome_bg(cx))
            .border_t_1()
            .border_color(color::border(cx))
            .child(
                gpui_component::h_flex()
                    .flex_none()
                    .items_center()
                    .gap(px(tokens::space::XXS))
                    .p(px(tokens::space::XXS))
                    .rounded(px(tokens::radius::MD))
                    .border_1()
                    .border_color(color::border(cx))
                    .bg(color::content_bg(cx))
                    .child(segment("Reclass", icon::struct_(), ViewMode::Tree, cx))
                    .child(segment("Code", icon::function(), ViewMode::Rendered, cx)),
            )
    }

    /// The body for the active tab: the editor (tree mode) or the rendered C/C++
    /// code view (rendered mode).
    ///
    /// The rendered side wires the fully-implemented codegen
    /// ([`render_cpp_tree`]) and presents it like reclass PIC3's right pane — a
    /// scrollable, read-only Zed code editor with a muted line-number gutter and
    /// One Dark syntax highlighting (see [`Self::render_code_view`]).
    fn render_body(&self, cx: &Context<Self>) -> AnyElement {
        let Some(entry) = self.active_entry() else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(color::text_muted(cx))
                .child("No document")
                .into_any_element();
        };

        match entry.view_mode {
            ViewMode::Tree => entry.editor.clone().into_any_element(),
            ViewMode::Rendered => self.render_code_view(entry, cx),
        }
    }

    /// The rendered C/C++ pane (reclass PIC3, right side) — a scrollable,
    /// read-only Zed code editor.
    ///
    /// Wires the real generator: it reads the active tab's editor → controller →
    /// tree + view root and calls [`render_cpp_tree`] with the document's
    /// [`TypeAliases`](crate::generator::TypeAliases) (the per-kind display-name
    /// overrides) and `emit_asserts = false`. The output is split into lines and
    /// rendered with a muted, right-aligned line-number gutter plus per-line One
    /// Dark syntax highlighting (keywords magenta, types yellow, numbers orange,
    /// strings green, trailing `// 0x..` comments dim green-gray).
    ///
    /// When there is no struct root (a fresh/empty document) the generator
    /// returns an empty string; we show a centered muted placeholder instead.
    fn render_code_view(&self, entry: &DocEntry, cx: &Context<Self>) -> AnyElement {
        let ed = entry.editor.read(cx);
        let tree = ed.controller().tree();
        let root = ed.controller().view_root_id();
        // The document's per-kind name overrides feed the renderer's type names.
        let aliases = &ed.controller().document().type_aliases;
        let aliases = if aliases.is_empty() {
            None
        } else {
            Some(aliases)
        };
        let source = render_cpp_tree(tree, root, aliases, /* emit_asserts */ false);

        // Empty (no struct root / non-struct view) → graceful placeholder.
        if source.trim().is_empty() {
            return div()
                .id("rcx-code-view-empty")
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(color::content_bg(cx))
                .text_color(color::text_muted(cx))
                .font_family(tokens::font::MONO_FAMILY)
                .text_size(px(tokens::font::EDITOR_SIZE))
                .child("// nothing to render — open or build a struct")
                .into_any_element();
        }

        // Gutter width grows with the line count so the digits stay right-aligned
        // and the source column never shifts (Zed gutter behaviour).
        let line_count = source.lines().count().max(1);
        let digits = ((line_count as f32).log10().floor() as usize) + 1;
        let gutter_w = px(digits as f32 * 8.5 + 24.0);
        let line_h = px(tokens::font::EDITOR_SIZE * tokens::font::EDITOR_LINE_HEIGHT);

        let gutter_fg = color::syntax_address(cx);

        let rows: Vec<AnyElement> = source
            .lines()
            .enumerate()
            .map(|(i, line)| self.render_code_line(i + 1, line, gutter_w, line_h, gutter_fg, cx))
            .collect();

        gpui_component::v_flex()
            .id("rcx-code-view")
            .size_full()
            .bg(color::content_bg(cx))
            .overflow_scroll()
            .font_family(tokens::font::MONO_FAMILY)
            .text_size(px(tokens::font::EDITOR_SIZE))
            .py(px(tokens::space::SM))
            .children(rows)
            .into_any_element()
    }

    /// One rendered source line: the muted right-aligned line-number gutter +
    /// the syntax-highlighted source spans (a per-line tokenizer pass).
    fn render_code_line(
        &self,
        number: usize,
        line: &str,
        gutter_w: Pixels,
        line_h: Pixels,
        gutter_fg: Hsla,
        cx: &Context<Self>,
    ) -> AnyElement {
        let spans = highlight_cpp_line(line, cx);

        gpui_component::h_flex()
            .w_full()
            .flex_none()
            .h(line_h)
            .items_center()
            .child(
                // Line-number gutter: muted, right-aligned, fixed width.
                div()
                    .flex_none()
                    .w(gutter_w)
                    .pr(px(tokens::space::LG))
                    .text_color(gutter_fg)
                    .child(div().w_full().text_right().child(number.to_string())),
            )
            .child(
                // Source column. The highlight spans must sit INLINE on one row,
                // so the column is itself a horizontal flex (a bare `div()`
                // defaults to block/column layout and stacks each `.flex_none()`
                // span vertically). A trailing space keeps a blank line from
                // collapsing to zero height; the per-span runs preserve leading
                // indentation as plain whitespace spans.
                gpui_component::h_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .items_center()
                    .pr(px(tokens::space::LG))
                    .whitespace_nowrap()
                    .children(if spans.is_empty() {
                        vec![div().child(" ").into_any_element()]
                    } else {
                        spans
                    }),
            )
            .into_any_element()
    }
}

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

/// A simple per-line C/C++ tokenizer → colored spans (One Dark).
///
/// Splits a line into word / number / string / comment / punctuation runs and
/// classifies each (keyword/type/number/string/comment) so the rendered pane
/// reads like a Zed code editor. Whitespace is preserved as plain spans so
/// indentation and column alignment survive.
fn highlight_cpp_line(line: &str, cx: &gpui::App) -> Vec<AnyElement> {
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

impl Panel for DocumentArea {
    fn panel_name(&self) -> &'static str {
        "DocumentArea"
    }

    /// The center document area must NOT show a dock-panel tab/title of its own —
    /// reclass (PIC1/PIC2/PIC5) and Zed both show a *single* tab row, and that row
    /// is the per-document strip rendered inside the panel body
    /// ([`render_tab_strip`](Self::render_tab_strip)), not the surrounding
    /// `DockArea` panel-tab. gpui-component's `TabPanel` always reserves a fixed
    /// title-bar strip for a single-panel center; we can't remove that strip from
    /// here, but we strip every scrap of chrome out of it so it reads as the same
    /// surface as the editor below (no "Documents" label, no active-tab underline,
    /// no zoom/menu button) — leaving the per-document strip as the only visible
    /// tab row. The bar's bg is blended to `content_bg` (see [`Self::title_style`]).
    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // An empty, zero-content title — no "Documents" text, no tab underline.
        div()
    }

    /// Blend the (unavoidable, gpui-component-fixed) single-panel title strip into
    /// the document surface so it carries no visible chrome of its own — the only
    /// tab row the user sees is the per-document strip in the body.
    fn title_style(&self, cx: &App) -> Option<TitleStyle> {
        Some(TitleStyle {
            background: color::content_bg(cx),
            foreground: color::text(cx),
        })
    }

    /// No zoom/menu affordance on the center panel — it would re-introduce dock
    /// chrome on the row we are deliberately keeping chrome-less.
    fn zoomable(&self, _cx: &App) -> Option<PanelControl> {
        None
    }

    fn closable(&self, _cx: &App) -> bool {
        // The center document area is never closed (it always holds a document).
        false
    }
}

impl EventEmitter<PanelEvent> for DocumentArea {}
impl EventEmitter<DocAreaEvent> for DocumentArea {}

impl Focusable for DocumentArea {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DocumentArea {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let strip = self.render_tab_strip(cx);
        let body = self.render_body(cx);
        let view_toggle = self.render_view_toggle(cx);

        // Top → bottom: the document tab strip, the active editor/rendered body,
        // then the Zed segmented "Reclass | Code" view-mode toggle (PIC5/PIC2).
        div()
            .id("rcx-document-area")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(color::content_bg(cx))
            .child(strip)
            .child(div().flex_1().min_h_0().child(body))
            .child(view_toggle)
    }
}

/// Map a [`SourceKind`] to its tab source-icon as a real SVG (the
/// `drawTabSourceIcon` per-tab provider badge; app-shell §8). Replaces the
/// round-2 text-glyph stand-in with a crisp [`design::icon`](icon) SVG.
///
/// Liveness mirrors `provider && provider->isValid()`: a live source paints at
/// the full tab foreground, a disconnected one dims to a muted, alpha-reduced
/// tint (the C++ ×0.40 live opacity) — the "No source" plug also reads dimmed.
fn source_icon(kind: SourceKind, live: bool, cx: &App) -> Icon {
    // Domain → SVG: a file is a document, a buffer/snapshot is memory/storage,
    // a process is the gear (matching the C++ ⚙), and "no source" reuses the
    // neutral data-store glyph rendered dimmed (the plug fallback).
    let svg = match kind {
        SourceKind::None => Icon::new(IconName::HardDrive),
        SourceKind::File => Icon::new(IconName::File),
        SourceKind::Buffer => icon::hex(), // MemoryStick — an in-memory buffer.
        SourceKind::Snapshot => icon::database(), // HardDrive — a captured store.
        SourceKind::Process => icon::settings(), // gear — a live process.
    };
    // None is never "live": always dim the plug fallback.
    let lit = live && kind != SourceKind::None;
    let tint = if lit {
        color::text(cx)
    } else {
        // Disconnected / no-source: muted, alpha-reduced (the C++ dim).
        color::text_disabled(cx)
    };
    svg.flex_none().text_color(tint).small()
}

/// A tiny extension so the area can map [`SourceKind`] → its tab source-icon
/// as a real SVG. (Kept as the public tab/icon hook for the titlebar + scanner.)
pub fn tab_source_icon(kind: SourceKind, live: bool, cx: &App) -> impl IntoElement {
    source_icon(kind, live, cx)
}

#[cfg(test)]
mod tests {
    // Pure state-transition tests for the tab model. The gpui-bound rendering is
    // covered by the build; here we test the active-index reducer logic directly
    // on a lightweight mirror of `DocumentArea`'s rules (the same invariants the
    // gpui methods enforce: active follows close, never blank, monotonic ids),
    // to keep them gpui-free and deterministic.
    use super::super::state::{DocId, ViewMode};

    /// A gpui-free mirror of `DocumentArea`'s tab list + active-index rules.
    struct TabModel {
        ids: Vec<DocId>,
        active: usize,
        next: u64,
    }
    impl TabModel {
        fn new() -> Self {
            let mut m = TabModel {
                ids: Vec::new(),
                active: 0,
                next: 0,
            };
            m.push();
            m
        }
        fn push(&mut self) -> DocId {
            self.next += 1;
            let id = DocId::from_raw(self.next);
            self.ids.push(id);
            self.active = self.ids.len() - 1;
            id
        }
        fn activate(&mut self, ix: usize) {
            if ix < self.ids.len() {
                self.active = ix;
            }
        }
        // Mirror of close_index: emit-less, but the same active fix-up rules.
        fn close(&mut self, ix: usize) {
            if ix >= self.ids.len() {
                return;
            }
            let was_active = ix == self.active;
            self.ids.remove(ix);
            if self.ids.is_empty() {
                self.push();
            } else {
                if was_active {
                    self.active = self.ids.len() - 1;
                } else if ix < self.active {
                    self.active -= 1;
                }
                self.active = self.active.min(self.ids.len() - 1);
            }
        }
    }

    #[test]
    fn starts_with_one_active_document() {
        let m = TabModel::new();
        assert_eq!(m.ids.len(), 1);
        assert_eq!(m.active, 0);
    }

    #[test]
    fn ids_are_monotonic_and_unique() {
        let mut m = TabModel::new();
        let a = m.ids[0];
        let b = m.push();
        let c = m.push();
        assert!(a.get() < b.get() && b.get() < c.get());
    }

    #[test]
    fn push_makes_new_tab_active() {
        let mut m = TabModel::new();
        let _b = m.push();
        assert_eq!(m.active, 1);
        let _c = m.push();
        assert_eq!(m.active, 2);
    }

    #[test]
    fn close_last_tab_opens_a_fresh_one() {
        // Never leave a blank area (app-shell §8/§20).
        let mut m = TabModel::new();
        let only = m.ids[0];
        m.close(0);
        assert_eq!(m.ids.len(), 1);
        // The fresh tab has a new id (not the closed one).
        assert_ne!(m.ids[0], only);
        assert_eq!(m.active, 0);
    }

    #[test]
    fn closing_active_reassigns_active() {
        let mut m = TabModel::new(); // [1]
        m.push(); // [1,2]
        m.push(); // [1,2,3], active=2
                  // Close the active last tab → active clamps to new last (index 1).
        m.close(2);
        assert_eq!(m.ids.len(), 2);
        assert_eq!(m.active, 1);

        // Re-grow, then close a tab *before* the active one → active shifts left.
        m.push(); // [.. , active=2]
        let before = m.active;
        m.close(0);
        assert_eq!(m.active, before - 1);
    }

    #[test]
    fn closing_non_active_keeps_active_document() {
        let mut m = TabModel::new(); // [1]
        m.push(); // [1,2]
        m.push(); // [1,2,3] active=2
        m.activate(2);
        let active_id = m.ids[2];
        // Close the first (non-active) tab.
        m.close(0);
        // The same document remains active.
        assert_eq!(m.ids[m.active], active_id);
    }

    #[test]
    fn view_mode_default_is_tree() {
        assert_eq!(ViewMode::default(), ViewMode::Tree);
    }
}
