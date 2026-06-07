//! Menu dispatch cluster — the command palette opener, the Alt-mnemonic toggle,
//! and the big `run_menu_command` router that maps every menu/command-id to its
//! action. Extracted from window.rs as an `impl super::MainWindow` block;
//! `use super::*` inherits the parent's imports.

use super::*;

/// Generate a thin global-key-binding handler routing a gpui action to its MENU
/// CONTRACT command via `run_menu_command`. Emits `pub(super)` so the generated
/// handlers stay visible to `MainWindow::new`'s `.on_action` registrations in the
/// parent module (the macro + its invocations live here with `run_menu_command`).
macro_rules! menu_action {
    ($name:ident, $action:ty, $cmd:literal) => {
        pub(super) fn $name(&mut self, _: &$action, window: &mut Window, cx: &mut Context<Self>) {
            self.run_menu_command(&$cmd.to_string(), window, cx);
        }
    };
}

impl super::MainWindow {
    pub(super) fn open_command_palette(
        &mut self,
        _: &OpenCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::pickers::commandpalette::{CommandPalette, PaletteEvent};
        let palette = cx.new(|cx| CommandPalette::new(window, cx));
        let focus = palette.read(cx).focus_handle(cx);
        self.palette_sub = Some(cx.subscribe_in(
            &palette,
            window,
            |this, _p, ev: &PaletteEvent, window, cx| match ev {
                PaletteEvent::Trigger(cmd) => {
                    let cmd = cmd.clone();
                    this.close_top_modal(window, cx);
                    this.run_menu_command(&cmd, window, cx);
                }
                PaletteEvent::Cancel => this.close_top_modal(window, cx),
            },
        ));
        // Render the palette in our OWN centered-modal overlay, not
        // `window.open_dialog` — whose `Dialog` focus_trap swallowed the palette's
        // Up/Down/Enter (the keyboard-nav-dead bug; see [`ModalEntry`]). The palette
        // sizes via `w_full` + `max_w(600)`, so give the card the 600px width the
        // dialog used to provide, and focus the palette's query input.
        self.open_centered_modal(palette.into(), focus, Some(px(600.)), window, cx);
    }

    // ── Global-key-binding action handlers (route to `run_menu_command`) ──────
    // Each maps a registered key binding to its MENU CONTRACT command so the
    // keyboard, the menu bar, and the command palette share one dispatch.

    menu_action!(on_refresh, RefreshView, "view.refresh");
    menu_action!(on_goto_address, GotoAddressAction, "view.goto_address");
    menu_action!(on_toggle_modules, ToggleModules, "view.modules");
    menu_action!(on_toggle_bookmarks, ToggleBookmarks, "view.bookmarks");
    menu_action!(on_split_editor, SplitEditor, "view.split");
    /// Toggle the top-level menu-bar menu whose Alt-mnemonic is `letter` (the
    /// Alt+letter accelerators, e.g. Alt+F ⇒ File). Opening it focuses its dropdown
    /// (via `MenuBar::render`), so Esc then closes it.
    pub(super) fn toggle_menu_mnemonic(&mut self, letter: char, cx: &mut Context<Self>) {
        self.menubar.update(cx, |mb, cx| {
            mb.activate_mnemonic(letter, cx);
        });
    }

    menu_action!(on_unsplit_editor, UnsplitEditor, "view.unsplit");

    // The File/Edit accelerator handlers — each routes its bound key to the same
    // MENU CONTRACT command `run_menu_command` dispatches (the blocker fix: these
    // accelerators were advertised in the menu but had no global key binding).
    menu_action!(on_new_class, NewClassAction, "file.new_class");
    menu_action!(on_new_struct, NewStructAction, "file.new_struct");
    menu_action!(on_new_enum, NewEnumAction, "file.new_enum");
    menu_action!(on_open_file, OpenFileAction, "file.open");
    menu_action!(on_save, SaveAction, "file.save");
    menu_action!(on_save_as, SaveAsAction, "file.save_as");
    menu_action!(on_close_doc, CloseDocAction, "file.close");
    menu_action!(on_undo, UndoAction, "edit.undo");
    menu_action!(on_redo, RedoAction, "edit.redo");
    menu_action!(on_add_bookmark, AddBookmarkAction, "edit.add_bookmark");
    menu_action!(on_quick_bookmark, QuickBookmarkAction, "edit.quick_bookmark");
    menu_action!(on_shortcuts, ShortcutsAction, "help.shortcuts");
    // The Tools accelerator handlers — the RTTI Browser (Ctrl+Shift+R;
    // main.cpp:1524) and Performance Profiler (Ctrl+Shift+F; main.cpp:1565) were
    // advertised in the Tools menu but had no global key binding, so both
    // shortcuts were dead. Each routes its bound key to the same MENU CONTRACT
    // command `run_menu_command` already dispatches.
    menu_action!(on_rtti, RttiAction, "tools.rtti");
    menu_action!(on_profiler, ProfilerAction, "tools.profiler");

    /// Dispatch a chosen command (from the menu bar, the command palette, or a
    /// global key binding). Maps a
    /// [`CommandId`](crate::ui::pickers::commandpalette::CommandId) to the app operation that
    /// realizes it — handling **every** command id in the MENU CONTRACT (File /
    /// Edit / View / Help). Commands whose deeper workflow has no logic yet
    /// (some Edit clipboard ops, split editor) degrade gracefully — they notify
    /// the user rather than panic or dead-end — so a menu/palette pick always
    /// *does* something visible.
    pub(super) fn run_menu_command(
        &mut self,
        cmd: &crate::ui::pickers::commandpalette::CommandId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match cmd.as_str() {
            // ── File: new documents (seed the per-kind root; the C++ newClass /
            // newStruct / newEnum each pass a distinct class keyword). ──
            "file.new_class" => self.new_document(RootKind::Class, window, cx),
            "file.new_struct" => self.new_document(RootKind::Struct, window, cx),
            "file.new_enum" => self.new_document(RootKind::Enum, window, cx),
            "file.welcome" => self.show_start_page(window, cx),

            // ── File: open / save ──
            "file.open" => self.prompt_open(window, cx),
            "file.save" => self.save_active(false, window, cx),
            "file.save_as" => self.save_active(true, window, cx),
            "file.close" => self.close_active_document(window, cx),
            "file.exit" => self.request_quit(window, cx),

            // ── File: import ──
            "file.import.source" | "import.source" => {
                self.prompt_import(ImportKind::Source, window, cx)
            }
            "file.import.xml" | "import.xml" => self.prompt_import(ImportKind::Xml, window, cx),
            "file.import.pdb" | "import.pdb" => self.prompt_import(ImportKind::Pdb, window, cx),

            // ── File: export (render the active tree, prompt a save path) ──
            "file.export.cpp" | "export.cpp" => self.export_code(ExportKind::Cpp, window, cx),
            "file.export.rust" | "export.rust" => self.export_code(ExportKind::Rust, window, cx),
            "file.export.defines" | "export.defines" => {
                self.export_code(ExportKind::Defines, window, cx)
            }
            "file.export.csharp" | "export.csharp" => {
                self.export_code(ExportKind::CSharp, window, cx)
            }
            "file.export.python" | "export.python" => {
                self.export_code(ExportKind::Python, window, cx)
            }
            "file.export.xml" | "export.xml" => self.export_code(ExportKind::Xml, window, cx),

            // ── File: data source (the active-source picker; data_options.png) ──
            // The C++ `m_sourceMenu` triggers route to controller->selectSource /
            // clearSources. File attaches a binary; Process opens the picker; the
            // remaining live providers have no factory on this platform.
            "source.clear" => self.clear_active_source(window, cx),
            "source.file" => self.prompt_data_file(window, cx),
            "source.process" => self.open_process_picker(window, cx),
            // The C++ Data Source set is File + the registered providers only — no
            // Kernel Memory row (kernelmemory is a Browse-Page-Tables provider id,
            // never a Data Source entry; see commandpalette.rs `menu_tree_with`).
            "source.remote" | "source.windbg" | "source.rcnet" => {
                self.report_unavailable_source(cmd.as_str(), window, cx)
            }

            // ── Edit (Undo / Redo / Add Bookmark… / Quick Bookmark Here) ──
            "edit.undo" => self.active_editor_undo(false, cx),
            "edit.redo" => self.active_editor_undo(true, cx),
            "edit.add_bookmark" => self.prompt_add_bookmark(window, cx),
            "edit.quick_bookmark" => self.quick_bookmark_here(window, cx),

            // ── View: docks / windows ──
            "view.project" => self.toggle_left_dock(window, cx),
            "view.scanner" => self.toggle_scanner_dock(&ToggleScanner, window, cx),
            "view.modules" => self.raise_modules(window, cx),
            "view.bookmarks" | "view.symbols" => self.raise_bookmarks(window, cx),
            "view.reset_windows" => self.reset_windows(window, cx),

            // ── View: editor view-option toggles (EDITOR SETTER CONTRACT) ──
            "view.compact_columns" => self.toggle_view_option(ViewOpt::CompactColumns, cx),
            "view.tree_lines" => self.toggle_view_option(ViewOpt::TreeLines, cx),
            "view.relative_offsets" => self.toggle_view_option(ViewOpt::RelativeOffsets, cx),
            "view.type_hints" => self.toggle_view_option(ViewOpt::TypeHints, cx),
            "view.comments" | "view.comment_chips" => {
                self.toggle_view_option(ViewOpt::ShowComments, cx)
            }
            "view.hover" | "view.hover_effects" => {
                self.toggle_view_option(ViewOpt::HoverEffects, cx)
            }
            "view.minimap" => self.toggle_view_option(ViewOpt::Minimap, cx),

            // ── View: font family (the C++ exclusive Consolas / JetBrains Mono
            // picker persisted to settings("font")). ──
            "view.font.consolas" => self.set_editor_font("Consolas", window, cx),
            "view.font.jetbrains" => self.set_editor_font("JetBrains Mono", window, cx),

            // ── View: actions ──
            "view.refresh" => self.refresh_active_editor(cx),
            "view.goto_address" => self.open_goto_address(window, cx),
            "view.command_palette" => self.open_command_palette(&OpenCommandPalette, window, cx),
            "view.split" => self.split_view(window, cx),
            "view.unsplit" => self.unsplit_view(window, cx),
            "view.presentation" => self.toggle_presentation(cx),
            // View ▸ Edit Theme — open the dedicated ThemeEditor (swatch grid /
            // live preview / save-as-user-copy), the C++ `editTheme`. Previously
            // this folded into the Options dialog and the ThemeEditor view had no
            // caller (item 4).
            "view.theme_edit" => self.open_theme_editor(window, cx),

            // ── Tools ──
            "tools.rtti" => self.open_rtti_browser(window, cx),
            "tools.type_aliases" => self.open_type_aliases_dialog(window, cx),
            "tools.mcp" => self.toggle_mcp(window, cx),
            "tools.options" => self.open_options_dialog(window, cx),
            "tools.profiler" => self.open_profiler_dialog(window, cx),

            // ── Plugins ──
            "plugins.manage" => self.open_plugins_dialog(window, cx),

            // ── Help ──
            "help.about" => self.show_about(window, cx),
            "help.shortcuts" | "help.docs" => self.show_shortcuts(window, cx),

            // The view-mode dual toggle (Tree ⇄ rendered C/C++) is exposed through
            // the titlebar; the menu has no direct entries for it, but the toggle
            // is reachable from the command palette via these synthetic ids.
            "view.tree" => self.set_active_view_mode(ViewMode::Tree, window, cx),
            "view.rendered" => self.set_active_view_mode(ViewMode::Rendered, window, cx),
            "view.debug" => self.set_active_view_mode(ViewMode::Debug, window, cx),

            // Theme by name (`view.theme.<NAME>`) — switch the active theme.
            other if other.starts_with("view.theme.") => {
                let name = &other["view.theme.".len()..];
                self.switch_theme_by_name(name, window, cx);
            }
            // A bundled example (`file.example.<NAME>`) — materialize + open it.
            other if other.starts_with("file.example.") => {
                let name = &other["file.example.".len()..];
                self.open_example(name, window, cx);
            }
            // A recent file (`file.recent.<INDEX>`) — reopen the recorded path.
            other if other.starts_with("file.recent.") => {
                self.open_recent_by_command(other, window, cx);
            }
            // Switch the active saved data source (`source.saved.<INDEX>`) — the
            // C++ `m_sourceMenu` saved-source rows route to
            // `controller->switchToSavedSource(idx)`. Previously these rows fell
            // into the catch-all and did nothing.
            other if other.starts_with("source.saved.") => {
                self.switch_saved_source_by_command(other, window, cx);
            }

            // A plugin-contributed command (design §6 Phase 2): route it through the
            // session-owned manager + a scoped live host. Checked AFTER the built-in
            // ids so a plugin can't shadow a host command, and BEFORE the catch-all
            // so it doesn't fall to the log-noop. In the default build no plugin
            // contributes a command, so this never matches.
            other if self.plugin_manager.is_plugin_command(other) => {
                self.dispatch_plugin_command(other, window, cx);
            }

            // ── Anything still unmapped: graceful, logged no-op. ──
            other => {
                tracing::debug!(command = %other, "menu command not yet wired");
            }
        }
    }
}
