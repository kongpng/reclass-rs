//! View / layout-control cluster — doc-load-into-active, the View-menu options
//! (toggle/set + apply-to-editor), Go-to-Address, theme switching + presentation
//! mode, active-source/undo, and the public dock accessors + dock/scanner/workspace
//! toggles. Extracted from window.rs as an `impl super::MainWindow` block;
//! `use super::*` inherits the parent's imports.

use super::*;

/// The seven checkable View-menu options (the C++ View menu defaults; the
/// `[check]` items in the MENU CONTRACT). All default **on** except comments.
/// `compact_columns`/`relative_offsets`/`hover_effects`/`minimap` are
/// render-level (the editor view); `tree_lines`/`type_hints`/`show_comments` are
/// compose flags (threaded through the controller's recompose).
#[derive(Clone, Copy, Debug)]
pub(crate) struct ViewOptions {
    pub(crate) compact_columns: bool,
    pub(crate) tree_lines: bool,
    pub(crate) relative_offsets: bool,
    pub(crate) type_hints: bool,
    pub(crate) show_comments: bool,
    pub(crate) hover_effects: bool,
    pub(crate) minimap: bool,
}

impl Default for ViewOptions {
    fn default() -> Self {
        // Match the C++ persisted QSettings defaults (main.cpp:1336-1411):
        //   compactColumns=true, treeLines=true, relativeOffsets=true,
        //   typeHints=FALSE, showComments=false, hoverEffects=true, minimap=FALSE.
        // The previous Rust defaults wrongly turned typeHints + minimap ON.
        ViewOptions {
            compact_columns: true,
            tree_lines: true,
            relative_offsets: true,
            type_hints: false,
            show_comments: false,
            hover_effects: true,
            minimap: false,
        }
    }
}

impl ViewOptions {
    /// Load the persisted view-option toggles from the disk store, falling back
    /// to the C++ defaults for any unset key (the C++ `settings.value(key,
    /// default).toBool()` pattern; main.cpp:1336-1411).
    pub(crate) fn load(store: &DiskSettings) -> Self {
        let d = ViewOptions::default();
        ViewOptions {
            compact_columns: store.get_bool(settings_keys::COMPACT_COLUMNS, d.compact_columns),
            tree_lines: store.get_bool(settings_keys::TREE_LINES, d.tree_lines),
            relative_offsets: store.get_bool(settings_keys::RELATIVE_OFFSETS, d.relative_offsets),
            type_hints: store.get_bool(settings_keys::TYPE_HINTS, d.type_hints),
            show_comments: store.get_bool(settings_keys::SHOW_COMMENTS, d.show_comments),
            hover_effects: store.get_bool(settings_keys::HOVER_EFFECTS, d.hover_effects),
            minimap: store.get_bool(settings_keys::MINIMAP, d.minimap),
        }
    }

    /// The QSettings key one option persists under (the C++ `setValue(key, …)`).
    pub(crate) fn key(opt: ViewOpt) -> &'static str {
        match opt {
            ViewOpt::CompactColumns => settings_keys::COMPACT_COLUMNS,
            ViewOpt::TreeLines => settings_keys::TREE_LINES,
            ViewOpt::RelativeOffsets => settings_keys::RELATIVE_OFFSETS,
            ViewOpt::TypeHints => settings_keys::TYPE_HINTS,
            ViewOpt::ShowComments => settings_keys::SHOW_COMMENTS,
            ViewOpt::HoverEffects => settings_keys::HOVER_EFFECTS,
            ViewOpt::Minimap => settings_keys::MINIMAP,
        }
    }

    pub(crate) fn get(&self, opt: ViewOpt) -> bool {
        match opt {
            ViewOpt::CompactColumns => self.compact_columns,
            ViewOpt::TreeLines => self.tree_lines,
            ViewOpt::RelativeOffsets => self.relative_offsets,
            ViewOpt::TypeHints => self.type_hints,
            ViewOpt::ShowComments => self.show_comments,
            ViewOpt::HoverEffects => self.hover_effects,
            ViewOpt::Minimap => self.minimap,
        }
    }

    pub(crate) fn set(&mut self, opt: ViewOpt, value: bool) {
        match opt {
            ViewOpt::CompactColumns => self.compact_columns = value,
            ViewOpt::TreeLines => self.tree_lines = value,
            ViewOpt::RelativeOffsets => self.relative_offsets = value,
            ViewOpt::TypeHints => self.type_hints = value,
            ViewOpt::ShowComments => self.show_comments = value,
            ViewOpt::HoverEffects => self.hover_effects = value,
            ViewOpt::Minimap => self.minimap = value,
        }
    }
}

/// The seven checkable editor view options, used to drive a single
/// [`MainWindow::toggle_view_option`] dispatch + the menu ✓ sync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewOpt {
    CompactColumns,
    TreeLines,
    RelativeOffsets,
    TypeHints,
    ShowComments,
    HoverEffects,
    Minimap,
}

impl ViewOpt {
    pub(crate) const ALL: [ViewOpt; 7] = [
        ViewOpt::CompactColumns,
        ViewOpt::TreeLines,
        ViewOpt::RelativeOffsets,
        ViewOpt::TypeHints,
        ViewOpt::ShowComments,
        ViewOpt::HoverEffects,
        ViewOpt::Minimap,
    ];

    /// The MENU CONTRACT command id whose ✓ this option drives.
    pub(crate) fn command_id(self) -> &'static str {
        match self {
            ViewOpt::CompactColumns => "view.compact_columns",
            ViewOpt::TreeLines => "view.tree_lines",
            ViewOpt::RelativeOffsets => "view.relative_offsets",
            ViewOpt::TypeHints => "view.type_hints",
            ViewOpt::ShowComments => "view.comments",
            ViewOpt::HoverEffects => "view.hover",
            ViewOpt::Minimap => "view.minimap",
        }
    }
}

impl super::MainWindow {
    /// Load an already-built document into the active editor tab and sync the
    /// tab title + source (shared by import; mirrors the tail of
    /// [`open_project`](Self::open_project)).
    pub(super) fn load_doc_into_active(
        &mut self,
        doc: crate::controller::RcxDocument,
        path: &std::path::Path,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source = Self::source_for_doc(&doc);
        let Some(active_id) = self.active_doc_id(cx) else {
            return;
        };
        if let Some(editor) = self.document_area.read(cx).active_editor().cloned() {
            editor.update(cx, |ed, cx| ed.set_document(doc, cx));
        }
        let title = Self::title_for_path(path);
        self.document_area.update(cx, |area, cx| {
            area.set_title(active_id, title.clone(), cx);
            area.set_source(active_id, source.clone(), cx);
        });
        self.state.set_title(active_id, title);
        self.state.set_source(active_id, source);
        self.rebuild_workspace(cx);
        self.dismiss_start_page(cx);
        cx.notify();
    }

    // ── View: editor view-option toggles (EDITOR SETTER CONTRACT) ────────────

    /// Flip a checkable View-menu option, push the new value into the active
    /// editor (render-level flags) or its controller (compose flags), and refresh
    /// the menu's ✓ to match. The window keeps a live mirror ([`ViewOptions`]) so
    /// the checkmark survives tab switches and reflects the C++ defaults.
    pub(super) fn toggle_view_option(&mut self, opt: ViewOpt, cx: &mut Context<Self>) {
        let value = !self.view_opts.get(opt);
        self.set_view_option_value(opt, value, cx);
    }

    /// Set a checkable View option to an explicit `value` (shared by
    /// [`toggle_view_option`] and the in-editor `ViewOptionToggled` event): push
    /// the value into EVERY open editor, mirror it into the window's `view_opts`,
    /// refresh the menu ✓, and persist it. Pushing to all panes is idempotent for
    /// the editor that originated an in-editor toggle.
    pub(super) fn set_view_option_value(&mut self, opt: ViewOpt, value: bool, cx: &mut Context<Self>) {
        self.view_opts.set(opt, value);
        // Push the new value into EVERY open editor via the EDITOR SETTER
        // CONTRACT (the C++ applies each view option to all open tabs, not just
        // the active one; main.cpp:1339-1411). The compose flags
        // (tree_lines/type_hints/show_comments) recompose; the render-level flags
        // (compact/relative/hover/minimap) repaint. The editor owns the actual
        // effect; the window owns the ✓.
        let editors: Vec<Entity<crate::ui::editor::RcxEditor>> = self
            .document_area
            .read(cx)
            .tabs()
            .iter()
            .map(|t| t.editor.clone())
            .collect();
        for editor in editors {
            editor.update(cx, |ed, cx| match opt {
                ViewOpt::CompactColumns => ed.set_compact_columns(value, cx),
                ViewOpt::TreeLines => ed.set_tree_lines(value, cx),
                ViewOpt::RelativeOffsets => ed.set_relative_offsets(value, cx),
                ViewOpt::TypeHints => ed.set_type_hints(value, cx),
                ViewOpt::ShowComments => ed.set_show_comments(value, cx),
                ViewOpt::HoverEffects => ed.set_hover_effects(value, cx),
                ViewOpt::Minimap => ed.set_minimap(value, cx),
            });
        }
        self.menubar.update(cx, |mb, cx| {
            mb.set_command_checked(opt.command_id(), value, cx);
        });
        // Persist the toggle to the disk store so it survives a relaunch (the
        // C++ `QSettings(...).setValue(key, checked)`; main.cpp:1336-1411).
        self.settings
            .borrow_mut()
            .set_bool(ViewOptions::key(opt), value);
        cx.notify();
    }

    /// Push the window's current [`view_opts`](Self::view_opts) into a single
    /// editor via the EDITOR SETTER CONTRACT. Fresh editors (the initial tab, a
    /// `new_document`, an `open_project`) get controller/view defaults that
    /// DISAGREE with the window's persisted view_opts, so without this a new tab
    /// ignores the current compact-columns/tree-lines/type-hints/comments/hover/
    /// minimap/relative-offsets state (the C++ applies every view option to all
    /// tabs). Calling this after `set_document` realigns the editor.
    pub(super) fn apply_view_opts_to_editor(
        &self,
        editor: &Entity<crate::ui::editor::RcxEditor>,
        cx: &mut Context<Self>,
    ) {
        let o = self.view_opts;
        let brace_wrap = self.brace_wrap;
        let refresh_ms = self.refresh_ms;
        // The persisted editor font family (the C++ applies the chosen font to
        // every editor; main.cpp). New editors start `font_family: None` (the
        // mono default), so without this push a fresh tab ignores the saved
        // View ▸ Font selection. An empty string means "no override" → mono.
        let font_family: Option<SharedString> = if self.editor_font.trim().is_empty() {
            None
        } else {
            Some(self.editor_font.clone().into())
        };
        editor.update(cx, |ed, cx| {
            ed.set_compact_columns(o.compact_columns, cx);
            ed.set_tree_lines(o.tree_lines, cx);
            ed.set_relative_offsets(o.relative_offsets, cx);
            ed.set_type_hints(o.type_hints, cx);
            ed.set_show_comments(o.show_comments, cx);
            ed.set_hover_effects(o.hover_effects, cx);
            ed.set_minimap(o.minimap, cx);
            ed.set_font_family(font_family.clone(), cx);
            // Generator brace-wrap + the persisted refresh interval are
            // controller-level (the C++ pushes both into every controller).
            ed.controller_mut().set_brace_wrap(brace_wrap);
            ed.controller_mut().set_refresh_interval(refresh_ms);
        });
    }

    /// View ▸ Go to Address… (Ctrl+G) — open the [`GotoAddressDialog`] in the
    /// dialog layer; on Go, re-resolve the formula against the active provider's
    /// callbacks and **navigate** the active editor to the resolved address (the
    /// C++ `showGotoAddressDialog` → `navigateToFormula`; main.cpp:4324). The
    /// dialog is seeded with the recent list + the active doc's pointer size, the
    /// accepted formula is pushed onto the recent list, and a failed resolve shows
    /// a themed modal warning (not a transient toast).
    pub(super) fn open_goto_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::ui::dialogs::gotoaddress::{GotoAddressDialog, GotoEvent};
        // Pointer size from the active document (32-bit projects deref correctly).
        let ptr_size = self
            .document_area
            .read(cx)
            .active_editor()
            .map(|ed| ed.read(cx).controller().tree().pointer_size)
            .filter(|&p| p > 0)
            .unwrap_or(8);
        let recent = self.goto_recent.clone();
        let dialog = cx.new(|cx| GotoAddressDialog::new(recent, ptr_size, window, cx));
        let focus = dialog.read(cx).focus_handle(cx);
        self.goto_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            |this, _d, ev: &GotoEvent, window, cx| match ev {
                GotoEvent::Go(formula, addr) => {
                    let formula = formula.clone();
                    let dialog_addr = *addr;
                    window.close_dialog(cx);
                    this.commit_goto(&formula, dialog_addr, window, cx);
                }
                GotoEvent::Cancel => window.close_dialog(cx),
            },
        ));
        self.present_modal(&dialog, 460., 120., Some(&focus), window, cx);
    }

    /// Resolve `formula` against the active provider's module/symbol/pointer
    /// callbacks and navigate the active editor to it (rebase its base address;
    /// the C++ `navigateToFormula`). On a clean resolve: push the recent list,
    /// rebase + refresh, confirm. On failure: themed warning. `dialog_addr` is
    /// the dialog's literal-only evaluation (used when no provider is attached).
    pub(super) fn commit_goto(
        &mut self,
        formula: &str,
        dialog_addr: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            return;
        };
        // Re-evaluate WITH the live provider callbacks so `<mod>+0x..` / `[ptr]` /
        // `ntdll!Sym` forms resolve (the dialog evaluated literals-only).
        let ptr_size = editor.read(cx).controller().tree().pointer_size.max(1);
        let resolved = {
            let ed = editor.read(cx);
            let provider = ed.controller().document().provider.clone();
            let cbs = crate::addr::AddressParserCallbacks {
                resolve_module: Some(Box::new({
                    let p = provider.clone();
                    move |name: &str| {
                        let base = p.symbol_to_address(name);
                        (base, base != 0)
                    }
                })),
                read_pointer: Some(Box::new({
                    let p = provider.clone();
                    move |addr: u64| {
                        if ptr_size >= 8 {
                            let v = p.read_u64(addr);
                            (v, true)
                        } else {
                            let v = p.read_u32(addr) as u64;
                            (v, true)
                        }
                    }
                })),
                resolve_identifier: Some(Box::new({
                    let p = provider.clone();
                    move |name: &str| {
                        let base = p.symbol_to_address(name);
                        (base, base != 0)
                    }
                })),
                ..Default::default()
            };
            let r = crate::addr::AddressParser::evaluate(formula, ptr_size, Some(&cbs));
            if r.ok {
                Some(r.value)
            } else if !formula.trim().is_empty() && dialog_addr != 0 {
                // Fall back to the dialog's literal evaluation (no provider).
                Some(dialog_addr)
            } else {
                None
            }
        };
        match resolved {
            Some(addr) => {
                // Navigate: rebase the active editor's tree to the resolved
                // address (the C++ `navigateToFormula` sets baseAddress and
                // preserves the formula for re-rebase), then recompose + repaint.
                editor.update(cx, |ed, cx| {
                    let tree = &mut ed.controller_mut().document_mut().tree;
                    tree.base_address = addr;
                    tree.base_address_formula = formula.to_string();
                    ed.apply_document(cx);
                });
                self.goto_recent = crate::ui::dialogs::gotoaddress::push_recent_list(&self.goto_recent, formula);
                // Persist the recent formulas across launches (the C++
                // `gotoAddress/recent` key) via the disk store.
                crate::ui::dialogs::gotoaddress::store_recent(
                    &mut *self.settings.borrow_mut(),
                    &self.goto_recent,
                );
                self.rebuild_workspace(cx);
                self.refresh_docks_for_active(cx);
                self.notify(format!("Jumped to 0x{addr:X}"), window, cx);
                cx.notify();
            }
            None => {
                let spec = crate::ui::dialogs::messagebox::warn(
                    "Address Not Resolved",
                    &format!(
                        "Couldn't evaluate \"{formula}\". The expression isn't valid or its \
                         module/symbol can't be resolved without a live data source."
                    ),
                );
                crate::ui::dialogs::messagebox::open_message(spec, window, cx);
            }
        }
    }

    /// Switch the active theme by display name (`view.theme.<NAME>`): find its
    /// index in the theme list and apply it, then check its menu row (clearing
    /// the others — the C++ exclusive `themeGroup`; main.cpp:1318). No-op
    /// (notified) if unknown.
    pub(super) fn switch_theme_by_name(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let index = self
            .theme_manager
            .borrow()
            .themes()
            .iter()
            .position(|t| t.name == name);
        match index {
            Some(i) => {
                self.switch_theme(i, window, cx);
                self.sync_theme_menu_checked(cx);
                // Persist the selection to the disk store ("theme" key) so it
                // survives a relaunch (the manager's own store is wiped each
                // launch); restored in the ctor.
                self.settings.borrow_mut().set(settings_keys::THEME, name);
            }
            None => self.notify(format!("Unknown theme: {name}"), window, cx),
        }
    }

    /// Mark the active theme's menu row checked and clear every other theme row
    /// (the C++ exclusive theme action group). Called after a theme switch and on
    /// first paint so the Theme submenu reflects reality.
    pub(super) fn sync_theme_menu_checked(&mut self, cx: &mut Context<Self>) {
        let active = self.state.theme_name().to_string();
        let names = crate::ui::pickers::commandpalette::theme_display_names();
        self.menubar.update(cx, |mb, cx| {
            for n in &names {
                mb.set_command_checked(&format!("view.theme.{n}"), *n == active, cx);
            }
        });
    }

    /// View ▸ Presentation Mode — toggle the presentation flag + its ✓ (the C++
    /// `setPresentationMode` on every editor + MCP slow-mode; main.cpp:1511). The
    /// editor surface exposes no presentation setter in this port, so the window
    /// owns the live flag + checkmark; editor-side presentation rendering + MCP
    /// slow-mode land with the editor's presentation pass.
    pub(super) fn toggle_presentation(&mut self, cx: &mut Context<Self>) {
        self.presentation = !self.presentation;
        let on = self.presentation;
        self.menubar.update(cx, |mb, cx| {
            mb.set_command_checked("view.presentation", on, cx);
        });
        // Slow the read cadence while presenting (the C++ presentation mode backs
        // off the refresh/MCP tick so the demo doesn't churn): in presentation the
        // controllers use a calm refresh interval; restoring leaves them at the
        // persisted interval. Mirror the blur-throttle path the window already owns.
        self.set_controllers_window_state(!on, true, cx);
        // Engage the editor spotlight on every open pane (the C++
        // `setPresentationMode(on)` on each editor; main.cpp:1511). This is the
        // central effect — the focus-glow + non-focused-row dimming — which was
        // dead because no pane's `set_presentation_mode` was ever called. Split
        // panes view the SAME active editor entity, so iterating the tab editors
        // covers every visible pane.
        let editors: Vec<Entity<crate::ui::editor::RcxEditor>> = self
            .document_area
            .read(cx)
            .tabs()
            .iter()
            .map(|t| t.editor.clone())
            .collect();
        for editor in editors {
            editor.update(cx, |ed, cx| ed.set_presentation_mode(on, cx));
        }
        // The render reads `self.presentation` to fade the chrome (titlebar +
        // status bar); request a repaint so the fade applies immediately.
        cx.notify();
    }

    /// Replace the active document's data source + reflect the tab icon
    /// (`source.clear` / source picks). Updates both the document area and state.
    pub(super) fn set_active_source(
        &mut self,
        source: crate::ui::state::DataSource,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.active_doc_id(cx) {
            self.document_area.update(cx, |area, cx| {
                area.set_source(id, source.clone(), cx);
            });
            self.state.set_source(id, source);
            // The provider changed — re-feed the docks + rebuild the Data Source
            // menu rows so the new (or cleared) source is reflected.
            self.refresh_docks_for_active(cx);
            self.rebuild_menus(cx);
            cx.notify();
        }
    }

    /// Undo / redo on the active editor (menu Edit▸Undo/Redo). No-op if no
    /// editor is active.
    pub(super) fn active_editor_undo(&mut self, redo: bool, cx: &mut Context<Self>) {
        if let Some(editor) = self.document_area.read(cx).active_editor().cloned() {
            editor.update(cx, |ed, cx| {
                if redo {
                    ed.redo(cx);
                } else {
                    ed.undo(cx);
                }
            });
            self.rebuild_workspace(cx);
            // Undo/redo changes the document's clean state — propagate the dirty
            // dot into the tab.
            self.sync_dirty_state(cx);
            cx.notify();
        }
    }

    /// The docking workspace entity.
    pub fn dock_area(&self) -> &Entity<DockArea> {
        &self.dock_area
    }

    /// The center document-tab area entity.
    pub fn document_area(&self) -> &Entity<DocumentArea> {
        &self.document_area
    }

    /// The current workspace layout-toggle preset.
    pub fn layout_preset(&self) -> LayoutPreset {
        self.layout_preset
    }

    /// Whether the start page is currently shown.
    pub fn start_page_visible(&self) -> bool {
        self.start_page.is_some()
    }

    // ── Workspace layout toggle (the C++ `applyLayoutPreset`; app-shell §10) ──

    /// Apply a workspace layout preset: show/hide the left dock and persist the
    /// toggle state (`applyLayoutPreset` — only the workspace dock is touched).
    pub fn apply_layout_preset(
        &mut self,
        preset: LayoutPreset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.layout_preset = preset;
        let want_open = preset.workspace_visible();
        self.set_left_dock_open(want_open, window, cx);
        cx.notify();
    }

    /// Open/close the left workspace dock to a specific state (drives the
    /// `set_open` on the underlying `Dock`).
    pub(super) fn set_left_dock_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        let dock_area = self.dock_area.clone();
        dock_area.update(cx, |area, cx| {
            if area.is_dock_open(DockPlacement::Left, cx) != open {
                if let Some(dock) = area.left_dock().cloned() {
                    dock.update(cx, |d, cx| d.set_open(open, window, cx));
                }
            }
        });
    }

    /// Sync the toggle preset from the left dock's actual visibility (the C++
    /// `setWorkspaceChecked` wired from `visibilityChanged`).
    pub fn sync_workspace_toggle(&mut self, cx: &mut Context<Self>) {
        let visible = self
            .dock_area
            .read(cx)
            .is_dock_open(DockPlacement::Left, cx);
        let preset = LayoutPreset::for_visible(visible);
        if preset != self.layout_preset {
            self.layout_preset = preset;
            cx.notify();
        }
    }

    // ── Memory-scanner pop-out (the C++ summoned-on-demand scanner window) ──

    /// `true` when the bottom memory-scanner dock is currently open. Drives the
    /// View ▸ Memory Scanner checkmark.
    pub fn scanner_open(&self, cx: &App) -> bool {
        self.dock_area
            .read(cx)
            .is_dock_open(DockPlacement::Bottom, cx)
    }

    /// Toggle the bottom memory-scanner dock open/closed (the C++ scanner pop-out,
    /// summoned via View ▸ Memory Scanner / `Ctrl+Shift+M`). The dock is closed by
    /// default (see [`docks::build_default_layout`]), so the first invocation shows
    /// it. Mirrors [`Self::set_left_dock_open`] but for [`DockPlacement::Bottom`],
    /// and refreshes the menu-bar checkmark so the menu reflects the new state.
    pub fn toggle_scanner_dock(
        &mut self,
        _: &ToggleScanner,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self.scanner_open(cx);
        self.set_bottom_dock_open(!open, window, cx);
        self.sync_scanner_menu_checked(cx);
        cx.notify();
    }

    /// Open/close the bottom scanner dock to a specific state (drives `set_open`
    /// on the underlying `Dock`). Mirrors [`Self::set_left_dock_open`].
    pub(super) fn set_bottom_dock_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        let dock_area = self.dock_area.clone();
        dock_area.update(cx, |area, cx| {
            if area.is_dock_open(DockPlacement::Bottom, cx) != open {
                if let Some(dock) = area.bottom_dock().cloned() {
                    dock.update(cx, |d, cx| d.set_open(open, window, cx));
                }
            }
        });
    }

    /// Push the scanner-open state into the menu bar so View ▸ Memory Scanner
    /// renders a checkmark while the dock is visible (the C++ checkable action).
    pub(super) fn sync_scanner_menu_checked(&mut self, cx: &mut Context<Self>) {
        let open = self.scanner_open(cx);
        self.menubar.update(cx, |mb, cx| {
            mb.set_command_checked("view.scanner", open, cx);
        });
    }

    // ── View mode (the dual tree/rendered toggle; app-shell §6/§16) ──

    /// Request a view mode on the active tab (the titlebar/strip toggle). Routes
    /// to the document area (which owns the per-tab mode) and updates state.
    pub fn set_active_view_mode(
        &mut self,
        mode: ViewMode,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.document_area.update(cx, |area, cx| {
            area.set_active_view_mode(mode, cx);
        });
        if let Some(id) = self.state.active_id() {
            self.state.set_view_mode(id, mode);
        }
        cx.notify();
    }

    // ── Start page (app-shell §13) ──
}
