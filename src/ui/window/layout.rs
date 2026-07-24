//! Layout cluster — the editor split-pane renderers (View ▸ Split Editor) and the
//! dock/workspace plumbing (left/right/scanner docks, raise-panel, split/unsplit,
//! reset windows, view-state sync). Extracted from window.rs as an
//! `impl super::MainWindow` block; `use super::*` inherits the parent's imports.

use super::*;

impl super::MainWindow {
    /// Always-visible 22px Project rail used while the left workspace dock is
    /// closed. This is intentionally outside `DockArea`: a closed dock otherwise
    /// has no hit target, while upstream reserves this narrow strip so Project is
    /// discoverable without opening the View menu.
    pub(super) fn render_workspace_rail(&self, cx: &Context<Self>) -> Option<AnyElement> {
        use crate::ui::design::{color, icon, tokens};
        use gpui_component::Sizable as _;

        if self
            .dock_area
            .read(cx)
            .is_dock_open(DockPlacement::Left, cx)
        {
            return None;
        }

        let letters = "PROJECT".chars().map(|ch| {
            div()
                .h(px(11.0))
                .flex()
                .items_center()
                .justify_center()
                .child(ch.to_string())
        });

        Some(
            gpui_component::v_flex()
                .id("rcx-workspace-collapsed-rail")
                .flex_none()
                .w(px(22.0))
                .h_full()
                .items_center()
                .border_r_1()
                .border_color(color::border(cx))
                .bg(color::chrome_bg(cx))
                .text_color(color::text_muted(cx))
                .cursor_pointer()
                .hover(|d| d.bg(color::hover_overlay(cx)).text_color(color::text(cx)))
                .on_click(cx.listener(|this, _e, window, cx| {
                    this.apply_layout_preset(LayoutPreset::Workspace, window, cx);
                }))
                .child(
                    div()
                        .flex_none()
                        .h(px(28.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon::chevron_right().with_size(px(12.0))),
                )
                .child(
                    gpui_component::v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .text_size(px(tokens::font::UI_XS))
                        .font_weight(FontWeight::SEMIBOLD)
                        .children(letters),
                )
                .into_any_element(),
        )
    }

    /// Build the editor split panes (View ▸ Split Editor) — one element per extra
    /// pane in [`split_panes`](Self::split_panes). Each pane views the SAME active
    /// document as the primary editor with its OWN view mode. The current Rust
    /// editor combines controller/document ownership with pane-local UI state, so
    /// extra Tree panes are read-only projections of the active editor; Rendered
    /// panes show generated source from that same controller. Each pane carries a
    /// header with a per-pane mode toggle and an "✕" that removes it. Returns an
    /// empty vec when unsplit (the primary pane is the dock area itself).
    pub(super) fn render_split_panes(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        use crate::ui::design::color;
        if self.split_panes.is_empty() {
            return Vec::new();
        }
        let editor = self.document_area.read(cx).active_editor().cloned();
        self.split_panes
            .iter()
            .enumerate()
            .map(|(pane_ix, pane)| {
                let inner = self.render_one_split_pane(pane_ix, pane, editor.as_ref(), cx);
                // Each pane is an equal-flex column with a left divider separating it
                // from the dock area / its sibling panes (Zed split gutter).
                gpui_component::v_flex()
                    .id(SharedString::from(format!("rcx-split-pane-{pane_ix}")))
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .border_l_1()
                    .border_color(color::border(cx))
                    .bg(color::content_bg(cx))
                    .child(inner)
                    .into_any_element()
            })
            .collect()
    }

    /// Render one split pane's header (a Tree/Code segmented toggle + remove "✕")
    /// stacked over its body (the live editor for Tree, the generated source for
    /// Rendered). Shared by [`render_split_panes`](Self::render_split_panes).
    pub(super) fn render_one_split_pane(
        &self,
        pane_ix: usize,
        pane: &SplitPaneState,
        editor: Option<&Entity<crate::ui::editor::RcxEditor>>,
        cx: &Context<Self>,
    ) -> AnyElement {
        use crate::ui::design::{color, tokens};
        let mode = pane.mode;

        // ── Header: per-pane view-mode segmented toggle + close button. ──
        let segment = |label: &'static str, this_mode: ViewMode, cx: &Context<Self>| {
            let selected = mode == this_mode;
            div()
                .id(SharedString::from(format!("split-seg-{pane_ix}-{label}")))
                .px(px(tokens::space::SM))
                .py(px(2.))
                .text_size(px(11.))
                .text_color(if selected {
                    color::text(cx)
                } else {
                    color::text_muted(cx)
                })
                .when(selected, |d| {
                    d.bg(color::selected_bg(cx)).rounded(px(tokens::radius::SM))
                })
                .hover(|d| d.bg(color::hover_overlay(cx)))
                .cursor_pointer()
                .child(label)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        // Land exactly on this segment's mode (a 3-mode control
                        // can't blindly toggle). Clicking the active segment is a
                        // no-op, like the C++ exclusive combo.
                        if this.split_panes.get(pane_ix).map(|pane| pane.mode) != Some(this_mode) {
                            this.set_split_pane_mode(pane_ix, this_mode, cx);
                        }
                    }),
                )
        };

        let header = gpui_component::h_flex()
            .flex_none()
            .h(px(28.))
            .w_full()
            .items_center()
            .justify_between()
            .px(px(tokens::space::SM))
            .bg(color::chrome_bg(cx))
            .border_b_1()
            .border_color(color::border(cx))
            .child(
                gpui_component::h_flex()
                    .gap(px(tokens::space::XXS))
                    .child(segment("Tree", ViewMode::Tree, cx))
                    .child(segment("Code", ViewMode::Rendered, cx))
                    .child(segment("Debug", ViewMode::Debug, cx))
                    .child(segment("Both", ViewMode::Both, cx)),
            )
            .child(
                gpui_component::h_flex()
                    .items_center()
                    .gap(px(tokens::space::SM))
                    .child(self.render_split_zoom_control(pane, cx))
                    .child(
                        div()
                            .id(SharedString::from(format!("split-close-{pane_ix}")))
                            .px(px(tokens::space::XS))
                            .text_size(px(13.))
                            .text_color(color::text_muted(cx))
                            .hover(|d| d.text_color(color::text(cx)))
                            .cursor_pointer()
                            .child("✕")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _e, _w, cx| {
                                    // Remove THIS pane (the C++ closes the pane's tab widget).
                                    if pane_ix < this.split_panes.len() {
                                        this.split_panes.remove(pane_ix);
                                        cx.notify();
                                    }
                                }),
                            ),
                    ),
            );

        // ── Body: the same document in the pane's view mode. ──
        //
        // Upstream already created a distinct RcxEditor per SplitPane, all bound to
        // one shared RcxController, before be9c2b6; that commit only reparented one
        // pane's existing editor + Code widgets into its new Both splitter. In this
        // port RcxEditor itself owns both RcxController and pane-local scroll,
        // focus, hover, edit, and zoom state. Reusing one Entity here would share
        // those supposedly pane-local states (and its element identity), while
        // constructing another RcxEditor creates a separate controller/document.
        // Until those layers are separated, extra Tree/Both surfaces remain
        // read-only projections over the authoritative active controller.
        let body = match (editor, mode) {
            (Some(ed), ViewMode::Tree) => self.render_split_tree(ed, pane.zoom_level, cx),
            (Some(ed), ViewMode::Rendered) => self.render_split_code(ed, pane.zoom_level, cx),
            (Some(ed), ViewMode::Debug) => self.render_split_debug(ed, pane.zoom_level, cx),
            (Some(ed), ViewMode::Both) => self.render_split_both(ed, pane, cx),
            (None, _) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(color::text_muted(cx))
                .child("No document")
                .into_any_element(),
        };

        gpui_component::v_flex()
            .size_full()
            .child(header)
            .child(div().flex_1().min_h_0().overflow_hidden().child(body))
            .into_any_element()
    }

    /// Continuous zoom slider/readout owned by one extra pane.
    fn render_split_zoom_control(&self, pane: &SplitPaneState, cx: &Context<Self>) -> AnyElement {
        use crate::ui::design::{color, tokens};
        use gpui_component::slider::Slider;
        let current = pane.zoom_level;
        let percent = view_zoom_percent(current, tokens::font::EDITOR_SIZE);

        gpui_component::h_flex()
            .items_center()
            .gap(px(tokens::space::XS))
            .text_size(px(10.))
            .text_color(color::text_muted(cx))
            .child("Zoom")
            .child(
                div().w(px(60.)).h(px(18.0)).flex().items_center().child(
                    Slider::new(&pane.zoom_slider)
                        .w_full()
                        .bg(color::border(cx))
                        .text_color(color::text_muted(cx)),
                ),
            )
            .child(
                div()
                    .min_w(px(32.))
                    .text_color(color::text(cx))
                    .child(format!("{percent}%")),
            )
            .into_any_element()
    }

    /// The read-only Tree projection for a split pane: the editor's last composed
    /// tree text (the C++ `SplitPane` tree view shows the same document). Rendered
    /// as monospaced lines so the split mirrors the primary editor's structure
    /// without re-rendering the live `RcxEditor` entity (which gpui only permits
    /// once per frame). The primary pane stays fully interactive; this is a faithful
    /// read-only mirror.
    pub(super) fn render_split_tree(
        &self,
        editor: &Entity<crate::ui::editor::RcxEditor>,
        zoom_level: i32,
        cx: &Context<Self>,
    ) -> AnyElement {
        use crate::ui::design::{color, tokens};
        let text = editor.read(cx).last_result().text.clone();
        if text.trim().is_empty() {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(color::content_bg(cx))
                .text_color(color::text_muted(cx))
                .font_family(tokens::font::mono_family())
                .text_size(px(tokens::font::EDITOR_SIZE))
                .child("// empty document")
                .into_any_element();
        }
        let font_size = tokens::font::EDITOR_SIZE + zoom_level as f32;
        let line_h = px(font_size * tokens::font::EDITOR_LINE_HEIGHT);
        let rows: Vec<AnyElement> = text
            .lines()
            .map(|line| {
                div()
                    .h(line_h)
                    .px(px(tokens::space::SM))
                    .text_color(color::text(cx))
                    .child(line.to_string())
                    .into_any_element()
            })
            .collect();
        gpui_component::v_flex()
            .id("rcx-split-tree-view")
            .size_full()
            .bg(color::content_bg(cx))
            .overflow_scroll()
            .font_family(tokens::font::mono_family())
            .text_size(px(font_size))
            .py(px(tokens::space::SM))
            .children(rows)
            .into_any_element()
    }

    /// The read-only Debug projection for a split pane (the C++ `SplitPane`
    /// `VM_Debug` view): the [`generate_debug_text`](crate::core::generate_debug_text)
    /// dump of the editor's last composed line/`LineMeta` model, **styled** per
    /// the C++ `styleDebugText` segmentation via the shared
    /// [`debug_styled_spans`](crate::ui::cpp_highlight::debug_styled_spans) helper so the split
    /// and primary debug panes agree exactly. A developer view of the existing
    /// structure — no live process, no editing — mirroring `render_split_tree` so
    /// the split never re-renders the live `RcxEditor` entity.
    pub(super) fn render_split_debug(
        &self,
        editor: &Entity<crate::ui::editor::RcxEditor>,
        zoom_level: i32,
        cx: &Context<Self>,
    ) -> AnyElement {
        use crate::ui::design::{color, tokens};
        let text = crate::core::generate_debug_text(editor.read(cx).last_result());
        if text.trim().is_empty() {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(color::content_bg(cx))
                .text_color(color::text_muted(cx))
                .font_family(tokens::font::mono_family())
                .text_size(px(tokens::font::EDITOR_SIZE))
                .child("// empty document")
                .into_any_element();
        }
        let font_size = tokens::font::EDITOR_SIZE + zoom_level as f32;
        let line_h = px(font_size * tokens::font::EDITOR_LINE_HEIGHT);
        let rows: Vec<AnyElement> = text
            .lines()
            .map(|line| {
                gpui_component::h_flex()
                    .h(line_h)
                    .px(px(tokens::space::SM))
                    .items_center()
                    .whitespace_nowrap()
                    .children(crate::ui::cpp_highlight::debug_styled_spans(line, cx))
                    .into_any_element()
            })
            .collect();
        gpui_component::v_flex()
            .id("rcx-split-debug-view")
            .size_full()
            .bg(color::content_bg(cx))
            .overflow_scroll()
            .font_family(tokens::font::mono_family())
            .text_size(px(font_size))
            .py(px(tokens::space::SM))
            .children(rows)
            .into_any_element()
    }

    /// The rendered-code projection for a split pane (the C++ `updateRenderedView`
    /// for a `SplitPane`): generate the source for the active editor's view root
    /// and show it as a read-only, line-numbered, monospaced text block. A
    /// self-contained mirror of the document area's code view (which lives in the
    /// out-of-ownership `tabs.rs`). It uses the same format/scope generator and
    /// format-aware highlighter as the primary Code/Both paths.
    pub(super) fn render_split_code(
        &self,
        editor: &Entity<crate::ui::editor::RcxEditor>,
        zoom_level: i32,
        cx: &Context<Self>,
    ) -> AnyElement {
        use crate::ui::design::{color, tokens};
        let (format, scope) = {
            let area = self.document_area.read(cx);
            (area.code_format(), area.code_scope())
        };
        let ed = editor.read(cx);
        let ctrl = ed.controller();
        let aliases = &ctrl.document().type_aliases;
        let aliases = if aliases.is_empty() {
            None
        } else {
            Some(aliases)
        };
        let root = crate::ui::chrome::tabs::rendered_root_for_controller(ctrl);
        let source = crate::generator::render_code_scoped(
            format,
            scope,
            ctrl.tree(),
            root,
            aliases,
            /* emit_asserts */ self.generator_asserts,
        );
        if source.trim().is_empty() {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(color::content_bg(cx))
                .text_color(color::text_muted(cx))
                .font_family(tokens::font::mono_family())
                .text_size(px(tokens::font::EDITOR_SIZE))
                .child("// nothing to render — open or build a struct")
                .into_any_element();
        }
        let font_size = tokens::font::EDITOR_SIZE + zoom_level as f32;
        let line_h = px(font_size * tokens::font::EDITOR_LINE_HEIGHT);
        let rows: Vec<AnyElement> = source
            .lines()
            .enumerate()
            .map(|(i, line)| {
                gpui_component::h_flex()
                    .h(line_h)
                    .items_center()
                    .child(
                        div()
                            .w(px(44.))
                            .pr(px(tokens::space::SM))
                            .text_align(gpui::TextAlign::Right)
                            .text_color(color::syntax_address(cx))
                            .child(format!("{}", i + 1)),
                    )
                    .child(
                        gpui_component::h_flex()
                            .flex_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .children(crate::ui::cpp_highlight::highlight_code_line(
                                line, format, cx,
                            )),
                    )
                    .into_any_element()
            })
            .collect();
        gpui_component::v_flex()
            .id("rcx-split-code-view")
            .size_full()
            .bg(color::content_bg(cx))
            .overflow_scroll()
            .font_family(tokens::font::mono_family())
            .text_size(px(font_size))
            .py(px(tokens::space::SM))
            .px(px(tokens::space::SM))
            .children(rows)
            .into_any_element()
    }

    /// Read-only 67/33 Tree + Code projection for an extra split pane. The live
    /// editor entity already renders in the primary pane, so both halves here are
    /// projections over the same active controller.
    pub(super) fn render_split_both(
        &self,
        editor: &Entity<crate::ui::editor::RcxEditor>,
        pane: &SplitPaneState,
        cx: &Context<Self>,
    ) -> AnyElement {
        use crate::ui::design::color;
        use gpui_component::resizable::{h_resizable, resizable_panel};

        h_resizable(SharedString::from(format!(
            "rcx-split-both-view-{}",
            pane.id
        )))
        .with_state(&pane.both_split)
        .child(
            resizable_panel()
                .w(relative(2.0 / 3.0))
                .size_range(px(120.0)..Pixels::MAX)
                .h_full()
                .child(self.render_split_tree(editor, pane.zoom_level, cx)),
        )
        .child(
            resizable_panel()
                .w(relative(1.0 / 3.0))
                .size_range(px(120.0)..Pixels::MAX)
                .h_full()
                .border_l_1()
                .border_color(color::border(cx))
                .child(self.render_split_code(editor, pane.zoom_level, cx)),
        )
        .into_any_element()
    }

    /// Feed the active document's provider into the scanner + modules docks and
    /// the active document's bookmark list into the bookmarks dock (the C++
    /// `ScannerPanel::set_provider` / `refreshModulesDock` / `refreshBookmarksDock`).
    /// Called whenever the active document or its source changes so the docks
    /// reflect reality instead of staying empty.
    pub(super) fn refresh_docks_for_active(&mut self, cx: &mut Context<Self>) {
        let editor = self.document_area.read(cx).active_editor().cloned();
        let Some(editor) = editor else {
            self.scanner.update(cx, |p, _| p.set_provider(None));
            self.modules.update(cx, |p, _| p.set_provider(None));
            self.target.update(cx, |p, _| {
                p.set_target(
                    None,
                    crate::ui::target_status::TargetStatusSummary::no_source(),
                )
            });
            self.bookmarks.update(cx, |p, cx| p.set_bookmarks(&[], cx));
            return;
        };
        let (provider, target_summary, bookmarks) = {
            let ed = editor.read(cx);
            let ctrl = ed.controller();
            (
                ctrl.document().provider.clone(),
                crate::ui::target_status::TargetStatusSummary::for_controller(ctrl),
                ctrl.document().tree.bookmarks.clone(),
            )
        };
        self.scanner
            .update(cx, |p, _| p.set_provider(Some(provider.clone())));
        self.modules
            .update(cx, |p, _| p.set_provider(Some(provider.clone())));
        self.target
            .update(cx, |p, _| p.set_target(Some(provider), target_summary));
        self.bookmarks
            .update(cx, |p, cx| p.set_bookmarks(&bookmarks, cx));
    }

    /// Push the window's focus/visibility into every open editor's controller so
    /// the adaptive refresh interval throttles on blur / pauses on minimize (the
    /// C++ `RcxController::setWindowState`; controller.cpp:5474). Called from the
    /// gpui window-activation observer.
    pub(super) fn set_controllers_window_state(
        &mut self,
        focused: bool,
        visible: bool,
        cx: &mut Context<Self>,
    ) {
        let editors: Vec<Entity<crate::ui::editor::RcxEditor>> = self
            .document_area
            .read(cx)
            .tabs()
            .iter()
            .map(|t| t.editor.clone())
            .collect();
        for editor in editors {
            editor.update(cx, |ed, _cx| {
                ed.controller_mut().set_window_state(focused, visible);
            });
        }
    }

    /// Push every checkable View-menu option's current state into the menu bar so
    /// the View menu's ✓ marks reflect reality (called on construction + after a
    /// layout reset). Mirrors [`sync_scanner_menu_checked`](Self::sync_scanner_menu_checked)
    /// for the editor view options, the docks, and presentation mode.
    pub(super) fn sync_view_menu_checked(&mut self, cx: &mut Context<Self>) {
        let opts = self.view_opts;
        let project_open = self
            .dock_area
            .read(cx)
            .is_dock_open(DockPlacement::Left, cx);
        let right_open = self
            .dock_area
            .read(cx)
            .is_dock_open(DockPlacement::Right, cx);
        let right_panel = self.right_dock_panel;
        let presentation = self.presentation;
        self.menubar.update(cx, |mb, cx| {
            for opt in ViewOpt::ALL {
                mb.set_command_checked(opt.command_id(), opts.get(opt), cx);
            }
            mb.set_command_checked("view.project", project_open, cx);
            // Modules / Bookmarks share the right dock, but each ✓ is tied to its
            // OWN panel's visibility (the C++ binds each ✓ to its dock; item 9).
            // A panel is "visible" only when the right dock is open AND that panel
            // is the raised tab — so opening Modules no longer also checks
            // Bookmarks (and vice versa).
            mb.set_command_checked(
                "view.modules",
                right_open && right_panel == RightDockPanel::Modules,
                cx,
            );
            mb.set_command_checked(
                "view.target",
                right_open && right_panel == RightDockPanel::Target,
                cx,
            );
            mb.set_command_checked(
                "view.bookmarks",
                right_open && right_panel == RightDockPanel::Bookmarks,
                cx,
            );
            mb.set_command_checked("view.presentation", presentation, cx);
        });
    }

    // ── View: docks ──────────────────────────────────────────────────────────

    /// Toggle the left workspace ("Project") dock (View ▸ Project). Mirrors the
    /// titlebar toggle but driven from the menu/palette, and re-syncs the ✓.
    pub(super) fn toggle_left_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self
            .dock_area
            .read(cx)
            .is_dock_open(DockPlacement::Left, cx);
        let preset = LayoutPreset::for_visible(!open);
        self.apply_layout_preset(preset, window, cx);
        self.sync_view_menu_checked(cx);
    }

    /// Toggle the **right** dock (Modules + Bookmarks, tabified together). Kept as
    /// the plain open/close primitive behind the more specific
    /// [`raise_modules`](Self::raise_modules) / [`raise_bookmarks`](Self::raise_bookmarks)
    /// (which open the dock AND focus their panel); a future "View ▸ Toggle Right
    /// Dock" command can route straight here.
    #[allow(dead_code)]
    pub(super) fn toggle_right_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self
            .dock_area
            .read(cx)
            .is_dock_open(DockPlacement::Right, cx);
        self.set_right_dock_open(!open, window, cx);
        self.sync_view_menu_checked(cx);
        cx.notify();
    }

    /// Open/close the right dock to a specific state (drives `set_open` on the
    /// underlying `Dock`). Mirrors [`set_bottom_dock_open`](Self::set_bottom_dock_open).
    pub(super) fn set_right_dock_open(
        &mut self,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dock_area = self.dock_area.clone();
        dock_area.update(cx, |area, cx| {
            if area.is_dock_open(DockPlacement::Right, cx) != open {
                if let Some(dock) = area.right_dock().cloned() {
                    dock.update(cx, |d, cx| d.set_open(open, window, cx));
                }
            }
        });
    }

    /// View ▸ Modules (`Ctrl+Shift+Y`) — raise the **Modules** tab of the right
    /// dock (the C++ `m_modulesDock->raise()`; main.cpp). Opens the right dock if
    /// closed and gives the Modules panel keyboard focus so it is the one the user
    /// lands on (gpui-component's `TabPanel` has no public "select tab N" API, so
    /// raising == open + focus the panel). Re-syncs the View ✓.
    pub(super) fn raise_modules(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Toggle, mirroring the C++ checkable Modules action whose toggled handler
        // hides the dock on re-invoke (main.cpp:1480-1497): if the right dock is
        // already showing Modules, hide it; otherwise open + raise + focus.
        let right_open = self
            .dock_area
            .read(cx)
            .is_dock_open(DockPlacement::Right, cx);
        if right_open && self.right_dock_panel == RightDockPanel::Modules {
            self.set_right_dock_open(false, window, cx);
            self.sync_view_menu_checked(cx);
            cx.notify();
            return;
        }
        self.set_right_dock_open(true, window, cx);
        // Record which right-dock tab is now active so the View ✓ for Modules
        // (and NOT Bookmarks) lights up (item 9).
        self.right_dock_panel = RightDockPanel::Modules;
        let focus = self.modules.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        self.sync_view_menu_checked(cx);
        cx.notify();
    }

    /// View ▸ Target — raise the **Target** tab of the right dock. The bottom
    /// status-bar target chip routes here too.
    pub(super) fn raise_target(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let right_open = self
            .dock_area
            .read(cx)
            .is_dock_open(DockPlacement::Right, cx);
        if right_open && self.right_dock_panel == RightDockPanel::Target {
            self.set_right_dock_open(false, window, cx);
            self.sync_view_menu_checked(cx);
            cx.notify();
            return;
        }
        self.set_right_dock_open(true, window, cx);
        self.right_dock_panel = RightDockPanel::Target;
        let focus = self.target.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        self.sync_view_menu_checked(cx);
        cx.notify();
    }

    /// View ▸ Bookmarks (`Ctrl+Shift+B`) — raise the **Bookmarks** tab of the right
    /// dock (the C++ `m_bookmarksDock->raise()`). Same open-+-focus behaviour as
    /// [`raise_modules`](Self::raise_modules) but targeting the Bookmarks panel.
    pub(super) fn raise_bookmarks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Toggle, mirroring the C++ checkable Bookmarks action (Qt toggleViewAction,
        // main.cpp:1499-1503): if the right dock is already showing Bookmarks, hide
        // it; otherwise open + raise + focus.
        let right_open = self
            .dock_area
            .read(cx)
            .is_dock_open(DockPlacement::Right, cx);
        if right_open && self.right_dock_panel == RightDockPanel::Bookmarks {
            self.set_right_dock_open(false, window, cx);
            self.sync_view_menu_checked(cx);
            cx.notify();
            return;
        }
        self.set_right_dock_open(true, window, cx);
        // Record which right-dock tab is now active so the View ✓ for Bookmarks
        // (and NOT Modules) lights up (item 9).
        self.right_dock_panel = RightDockPanel::Bookmarks;
        let focus = self.bookmarks.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        self.sync_view_menu_checked(cx);
        cx.notify();
    }

    /// View ▸ Split Editor (`Ctrl+\`) — append a split pane to the active document
    /// (the C++ `splitView` → `tab->panes.append(createSplitPane(*tab))`;
    /// main.cpp:4299). The new pane views the SAME active document and starts in
    /// the **rendered C/C++** mode (so the split is immediately useful: tree on the
    /// left, generated source on the right — the canonical reclass split). Capped
    /// to keep the layout legible. No-op (notified) with no active document.
    pub(super) fn split_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.document_area.read(cx).active_editor().is_none() {
            self.notify("Open a document before splitting the editor.", window, cx);
            return;
        }
        // The C++ has no hard cap, but more than two extra panes is unreadable in
        // a single row; cap at 2 extra (3 total incl. the primary) so the layout
        // stays usable. A capped split notifies rather than silently no-opping.
        const MAX_EXTRA_PANES: usize = 2;
        if self.split_panes.len() >= MAX_EXTRA_PANES {
            self.notify("Editor is already split to the maximum.", window, cx);
            return;
        }
        // New panes default to the rendered (code) view — the primary keeps the
        // tree, so the user gets the side-by-side tree⇄code split out of the box.
        self.next_split_pane_id += 1;
        let pane_id = self.next_split_pane_id;
        let zoom_level = self.view_zoom_level;
        let zoom_slider = cx.new(|_| {
            gpui_component::slider::SliderState::new()
                .min(VIEW_ZOOM_MIN as f32)
                .max(VIEW_ZOOM_MAX as f32)
                .step(1.0)
                .default_value(zoom_level as f32)
        });
        let zoom_subscription = cx.subscribe(
            &zoom_slider,
            move |this, _slider, event: &gpui_component::slider::SliderEvent, cx| {
                if let gpui_component::slider::SliderEvent::Change(value) = event {
                    this.set_split_pane_zoom_by_id(pane_id, value.start().round() as i32, cx);
                }
            },
        );
        let both_split = cx.new(|_| gpui_component::resizable::ResizableState::default());
        self.split_panes.push(SplitPaneState {
            id: pane_id,
            mode: ViewMode::Rendered,
            zoom_level,
            zoom_slider,
            _zoom_subscription: zoom_subscription,
            both_split,
        });
        cx.notify();
    }

    /// View ▸ Unsplit Editor (`Ctrl+Shift+\`) — remove the last split pane (the C++
    /// `unsplitView` → `tab->panes.takeLast()` guarded by `panes.size() > 1`;
    /// main.cpp:4305). With no extra panes the editor is already unsplit; notify
    /// rather than silently no-op.
    pub(super) fn unsplit_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.split_panes.pop().is_none() {
            self.notify("Editor is not split.", window, cx);
            return;
        }
        cx.notify();
    }

    /// Toggle the view mode of a split pane (the C++ per-`SplitPane` view-mode
    /// cycle). Retained for any non-segment caller; the segmented header uses
    /// [`set_split_pane_mode`](Self::set_split_pane_mode) so a click lands on its
    /// exact mode (a 3-mode control can't blindly toggle). No-op if `pane_ix` is
    /// stale.
    #[allow(dead_code)]
    pub(super) fn toggle_split_pane_mode(&mut self, pane_ix: usize, cx: &mut Context<Self>) {
        if let Some(pane) = self.split_panes.get_mut(pane_ix) {
            pane.mode = pane.mode.toggled();
            cx.notify();
        }
    }

    /// Set a split pane's view mode to an explicit target (the per-segment setter
    /// the split header's segmented control wires). Lands exactly on `mode`
    /// rather than cycling. No-op if `pane_ix` is stale or the mode is unchanged.
    pub(super) fn set_split_pane_mode(
        &mut self,
        pane_ix: usize,
        mode: ViewMode,
        cx: &mut Context<Self>,
    ) {
        if let Some(pane) = self.split_panes.get_mut(pane_ix) {
            if pane.mode != mode {
                pane.mode = mode;
                cx.notify();
            }
        }
    }

    /// Apply zoom to exactly one extra pane. The persisted value remains the seed
    /// for subsequently-created panes, while every existing pane keeps its own
    /// independent slider and projection size.
    fn set_split_pane_zoom_by_id(&mut self, pane_id: u64, level: i32, cx: &mut Context<Self>) {
        let level = clamp_view_zoom(level);
        let Some(pane) = self.split_panes.iter_mut().find(|pane| pane.id == pane_id) else {
            return;
        };
        if pane.zoom_level == level {
            return;
        }
        pane.zoom_level = level;
        self.view_zoom_level = level;
        self.settings
            .borrow_mut()
            .set(settings_keys::VIEW_ZOOM_LEVEL, &level.to_string());
        cx.notify();
    }

    /// Apply zoom to the active document's primary pane. Existing extra panes keep
    /// their own zoom values; the persisted value seeds future panes/documents.
    pub(super) fn set_primary_view_zoom_level(
        &mut self,
        level: i32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let level = clamp_view_zoom(level);
        self.view_zoom_level = level;
        self.document_area.update(cx, |area, cx| {
            area.set_default_view_zoom_level(level, cx);
            area.set_active_view_zoom_level(level, window, cx);
        });
        self.settings
            .borrow_mut()
            .set(settings_keys::VIEW_ZOOM_LEVEL, &level.to_string());
        cx.notify();
    }

    /// View ▸ Reset Windows — restore the canonical dock layout (the C++ "Reset
    /// Windows": discard the current placement and return every dock to its
    /// default size + open state). Restores the workspace dock open at its default
    /// width, the scanner + right docks closed at their default sizes, collapses
    /// any editor split, and re-syncs every menu ✓.
    ///
    /// This resets **placement only** — open documents, the active tab, and each
    /// panel's content are preserved (the C++ Reset Windows likewise re-docks the
    /// existing widgets, it does not reload the project). Default sizes mirror
    /// [`docks::build_default_layout`] (workspace ~280px, scanner ~320px).
    pub(super) fn reset_windows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Collapse any editor split back to the single primary pane.
        self.split_panes.clear();

        // Restore each dock to its canonical open state + default size. The
        // `apply_layout_preset` re-opens the workspace dock; the explicit size +
        // open resets below return the scanner/right docks to their default
        // geometry even after a drag-resize or close.
        self.apply_layout_preset(LayoutPreset::Workspace, window, cx);
        let dock_area = self.dock_area.clone();
        dock_area.update(cx, |area, cx| {
            if let Some(dock) = area.left_dock().cloned() {
                dock.update(cx, |d, cx| {
                    d.set_size(px(280.), window, cx);
                    d.set_open(true, window, cx);
                });
            }
            if let Some(dock) = area.bottom_dock().cloned() {
                dock.update(cx, |d, cx| {
                    d.set_size(px(320.), window, cx);
                    d.set_open(false, window, cx);
                });
            }
            if let Some(dock) = area.right_dock().cloned() {
                dock.update(cx, |d, cx| {
                    d.set_size(px(280.), window, cx);
                    d.set_open(false, window, cx);
                });
            }
        });
        self.layout_preset = LayoutPreset::Workspace;

        self.sync_scanner_menu_checked(cx);
        self.sync_view_menu_checked(cx);
        self.notify("Windows reset to the default layout.", window, cx);
        cx.notify();
    }

    // ── View: refresh / goto / theme / presentation ──────────────────────────

    /// View ▸ Refresh (F5) — reset the changed-byte heat tracking, then force the
    /// active editor to recompose + repaint (the C++ `resetChangeTracking()` then
    /// `refresh()`; main.cpp:1418). The previous version only recomposed, so the
    /// changed-byte highlight never cleared on refresh.
    pub(super) fn refresh_active_editor(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.document_area.read(cx).active_editor().cloned() {
            editor.update(cx, |ed, cx| {
                ed.controller_mut().reset_change_tracking();
                ed.apply_document(cx);
            });
            self.rebuild_workspace(cx);
            cx.notify();
        }
    }
}
