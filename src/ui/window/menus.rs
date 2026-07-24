//! Menu dispatch cluster — the command palette opener, the Alt-mnemonic toggle,
//! and the big `run_menu_command` router that maps every menu/command-id to its
//! action. Extracted from window.rs as an `impl super::MainWindow` block;
//! `use super::*` inherits the parent's imports.

use super::*;

/// Relabel the first leaf with the given command id, in place (used for the
/// dynamic MCP Start/Stop label). Recurses into submenus.
pub(crate) fn relabel_command(
    nodes: &mut [crate::ui::pickers::commandpalette::MenuNode],
    command: &str,
    new_label: &str,
) {
    use crate::ui::pickers::commandpalette::MenuNode;
    for node in nodes {
        match node {
            MenuNode::Item {
                label, command: c, ..
            } if c.as_str() == command => {
                *label = new_label.to_string();
                return;
            }
            MenuNode::Submenu { children, .. } => {
                relabel_command(children, command, new_label);
            }
            _ => {}
        }
    }
}

/// Inject one menu item per enabled plugin `Command` whose slot surfaces in the
/// menu bar (`Menu`/`SourceMenu`/`Palette`) into the `&Plugins` submenu's children
/// (design §6 Phase 2). Pure (no gpui) so the byte-identical-when-empty parity is
/// unit-testable: with an empty `commands` list the tree is returned untouched
/// (the `&Plugins` submenu keeps exactly its static `[Manage Plugins…]` row).
/// `EditorContext`/`Toolbar` slots are not menu-bar surfaces, so they are skipped
/// here (they'd be injected into the editor context menu / toolbar instead — those
/// surfaces are intended-deferred for plugin contributions).
pub(crate) fn inject_plugin_menu_items(
    tree: &mut [crate::ui::pickers::commandpalette::MenuNode],
    commands: &[crate::plugin::UiContribution],
) {
    use crate::plugin::{CommandSlot, UiContribution};
    use crate::ui::pickers::commandpalette::MenuNode;
    // Find the &Plugins submenu by its label (the static menu tree carries it).
    let Some(MenuNode::Submenu { children, .. }) = tree
        .iter_mut()
        .find(|n| matches!(n, MenuNode::Submenu { label, .. } if label == "&Plugins"))
    else {
        return;
    };
    for c in commands {
        if let UiContribution::Command { id, title, slot } = c {
            if matches!(
                slot,
                CommandSlot::Menu | CommandSlot::SourceMenu | CommandSlot::Palette
            ) {
                children.push(MenuNode::item(title, "", id));
            }
        }
    }
}

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
    menu_action!(on_toggle_target, ToggleTarget, "view.target");
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
    menu_action!(
        on_break_into_class,
        BreakIntoClassAction,
        "edit.break_into_class"
    );
    menu_action!(on_add_bookmark, AddBookmarkAction, "edit.add_bookmark");
    menu_action!(
        on_quick_bookmark,
        QuickBookmarkAction,
        "edit.quick_bookmark"
    );
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
            // clearSources. File attaches a binary; Process attaches a live target
            // through the first-party provider set; platform-specific providers
            // surface their own attach errors when unavailable on this OS.
            "source.clear" => self.clear_active_source(window, cx),
            "source.file" => self.prompt_data_file(window, cx),
            "source.process" => self.open_process_source(window, cx),
            #[cfg(feature = "memflow-provider")]
            "source.memflow" => self.open_memflow_attach_dialog(window, cx),
            #[cfg(feature = "remote-process-provider")]
            "source.remote" => self.open_provider_process_picker("remoteprocessmemory", window, cx),
            #[cfg(not(feature = "remote-process-provider"))]
            "source.remote" => self.report_unavailable_source(cmd.as_str(), window, cx),
            #[cfg(all(windows, feature = "kernel-provider"))]
            "source.kernel" => self.open_provider_process_picker("kernelmemory", window, cx),
            #[cfg(not(all(windows, feature = "kernel-provider")))]
            "source.kernel" => self.report_unavailable_source(cmd.as_str(), window, cx),
            #[cfg(all(windows, feature = "windbg-provider"))]
            "source.windbg" => self.open_windbg_attach_dialog(window, cx),
            #[cfg(not(all(windows, feature = "windbg-provider")))]
            "source.windbg" => self.report_unavailable_source(cmd.as_str(), window, cx),
            "source.rcnet" => self.report_unavailable_source(cmd.as_str(), window, cx),

            // ── Edit (Undo / Redo / Add Bookmark… / Quick Bookmark Here) ──
            "edit.undo" => self.active_editor_undo(false, cx),
            "edit.redo" => self.active_editor_undo(true, cx),
            "edit.break_into_class" => {
                if let Some(editor) = self.document_area.read(cx).active_editor().cloned() {
                    editor.update(cx, |editor, cx| {
                        editor.break_current_selection_into_class(cx)
                    });
                } else {
                    self.notify("Break: no document is open.", window, cx);
                }
            }
            "edit.add_bookmark" => self.prompt_add_bookmark(window, cx),
            "edit.quick_bookmark" => self.quick_bookmark_here(window, cx),

            // ── View: docks / windows ──
            #[cfg(windows)]
            "view.show_console" => self.toggle_console(cx),
            "view.project" => self.toggle_left_dock(window, cx),
            "view.scanner" => self.toggle_scanner_dock(&ToggleScanner, window, cx),
            "view.modules" => self.raise_modules(window, cx),
            "view.target" => self.raise_target(window, cx),
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
            "view.auto_rtti" => self.toggle_view_option(ViewOpt::AutoRtti, cx),
            "view.enum_chips" => self.toggle_view_option(ViewOpt::EnumChips, cx),
            "view.hover" | "view.hover_effects" => {
                self.toggle_view_option(ViewOpt::HoverEffects, cx)
            }
            "view.value_popups" => self.toggle_view_option(ViewOpt::ValuePopups, cx),
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
            "view.both" => self.set_active_view_mode(ViewMode::Both, window, cx),

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
            // so it doesn't fall to the log-noop. With no contributed plugin command
            // loaded, this never matches.
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

// Recent-files persistence + the menu (re)builder + saved-source command switch
// — kept with the menu dispatch above.
impl super::MainWindow {
    // ── Recent files (the C++ recentFiles QSettings list) ──

    /// Record an opened project path as the most-recent (the C++ `addRecentFile`;
    /// main.cpp:8765): dedup, most-recent-first, capped at 10. Rebuilds the menus
    /// + the start-page list.
    pub(super) fn record_recent_file(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.recent_files.retain(|p| p != &abs);
        self.recent_files.insert(0, abs);
        self.recent_files.truncate(10);
        // Persist the list to the disk store (the C++ `addRecentFile` →
        // `settings.setValue("recentFiles", recent)`; main.cpp:8765) so Open
        // Recent survives a relaunch. Loaded back in the ctor.
        self.persist_recent_files();
        self.rebuild_menus(cx);
    }

    /// Write the in-memory recent-files list to the disk store as a
    /// `\n`-joined `QStringList` (the C++ `recentFiles` key).
    pub(super) fn persist_recent_files(&self) {
        let values: Vec<String> = self
            .recent_files
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        self.settings
            .borrow_mut()
            .set_list(settings_keys::RECENT_FILES, &values);
    }

    /// The recent-files paths that still exist on disk, most-recent-first. Both
    /// the Recent Files submenu and the start page skip entries whose file no
    /// longer exists (the C++ `updateRecentFilesMenu` `if (!QFile::exists(path))
    /// continue;`; main.cpp:8789). Indices map back into the stored vec so a
    /// reopen targets the right path.
    pub(super) fn existing_recent_files(&self) -> Vec<(usize, &std::path::PathBuf)> {
        self.recent_files
            .iter()
            .enumerate()
            .filter(|(_, p)| p.exists())
            .collect()
    }

    /// Reopen a recent file from its `file.recent.<index>` command id.
    pub(super) fn open_recent_by_command(
        &mut self,
        cmd: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if cmd == "file.recent.empty" {
            return;
        }
        let Some(idx) = cmd
            .strip_prefix("file.recent.")
            .and_then(|s| s.parse::<usize>().ok())
        else {
            return;
        };
        let Some(path) = self.recent_files.get(idx).cloned() else {
            return;
        };
        self.open_project(&path, None, window, cx);
    }

    /// Switch the active saved data source from a `source.saved.<index>` command
    /// id (the C++ `m_sourceMenu` saved-source row → `switchToSavedSource(idx)`).
    /// Recomposes the editor, re-derives the tab source icon, re-feeds the docks,
    /// and rebuilds the menus so the new active row is checked.
    pub(super) fn switch_saved_source_by_command(
        &mut self,
        cmd: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = cmd
            .strip_prefix("source.saved.")
            .and_then(|s| s.parse::<i32>().ok())
        else {
            return;
        };
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            return;
        };
        let mut attach_error = None;
        {
            let plugin_manager = &self.plugin_manager;
            editor.update(cx, |ed, cx| {
                let result = ed
                    .controller_mut()
                    .switch_to_saved_source_with_provider_factory(idx, |identifier, target| {
                        plugin_manager.create_provider(identifier, target)
                    });
                if let Err(err) = result {
                    attach_error = Some(err);
                }
                ed.apply_document(cx);
            });
        }
        if let Some(err) = attach_error {
            self.notify(format!("Saved source attach failed: {err}"), window, cx);
        }
        // Re-derive the tab source icon + re-feed the docks from the new source.
        let source = Self::source_for_controller(editor.read(cx).controller());
        if let Some(active_id) = self.active_doc_id(cx) {
            self.document_area.update(cx, |area, cx| {
                area.set_source(active_id, source.clone(), cx);
            });
            self.state.set_source(active_id, source);
        }
        self.refresh_docks_for_active(cx);
        self.rebuild_menus(cx);
        cx.notify();
    }

    // ── Dynamic menu rebuild (the C++ aboutToShow rebuilders) ──

    /// Rebuild the menu tree with the live Recent-Files + Data-Source rows and the
    /// dynamic MCP Start/Stop label, then push it into the menu bar (the C++
    /// `updateRecentFilesMenu` / `populateSourceMenu` / MCP label flip). Preserves
    /// the checkmark state (held separately on the menu bar).
    pub(super) fn rebuild_menus(&mut self, cx: &mut Context<Self>) {
        use crate::ui::pickers::commandpalette::{
            menu_tree_with, RecentMenuEntry, SourceMenuEntry,
        };
        // Skip entries whose file no longer exists (the C++
        // `updateRecentFilesMenu` exists-filter); the command carries the
        // ORIGINAL stored index so a reopen targets the right path.
        let existing = self.existing_recent_files();
        // Two recent entries can be different files that share a base name
        // (e.g. /tmp/parity/png.rcx vs /tmp/example/png.rcx). The C++ leans on a
        // per-action tooltip to disambiguate (main.cpp:8793), but this port's
        // menu rows have no hover tooltip, so a bare "png.rcx" twice is visually
        // identical. When a file name repeats among the visible entries, append a
        // parent-directory hint so each row is distinguishable, e.g.
        // "png.rcx — parity" / "png.rcx — example".
        let mut name_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        let mut hint_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for (_, p) in &existing {
            if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                *name_counts.entry(name).or_insert(0) += 1;
            }
            if let Some(dir) = p
                .parent()
                .and_then(|d| d.file_name())
                .and_then(|s| s.to_str())
            {
                *hint_counts.entry(dir).or_insert(0) += 1;
            }
        }
        let recent: Vec<RecentMenuEntry> = existing
            .into_iter()
            .map(|(i, p)| {
                let file_name = p.file_name().and_then(|s| s.to_str()).unwrap_or("(file)");
                let label = if name_counts.get(file_name).copied().unwrap_or(0) > 1 {
                    // Prefer the short parent-dir name as the hint, but if that
                    // name itself is shared by another visible entry it would not
                    // disambiguate — fall back to the full parent path.
                    let short = p
                        .parent()
                        .and_then(|d| d.file_name())
                        .and_then(|s| s.to_str());
                    let hint = match short {
                        Some(dir) if hint_counts.get(dir).copied().unwrap_or(0) <= 1 => {
                            Some(dir.to_string())
                        }
                        _ => p.parent().map(|d| d.to_string_lossy().into_owned()),
                    };
                    match hint {
                        Some(h) => format!("{file_name} \u{2014} {h}"),
                        None => file_name.to_string(),
                    }
                } else {
                    file_name.to_string()
                };
                RecentMenuEntry {
                    label,
                    command: format!("file.recent.{i}"),
                }
            })
            .collect();
        // Saved sources from the active document's controller (the active one is
        // rendered checked via the host's checked-set).
        let sources: Vec<SourceMenuEntry> = self
            .document_area
            .read(cx)
            .active_editor()
            .map(|ed| {
                let ctrl = ed.read(cx).controller();
                let active = ctrl.active_source_index();
                ctrl.saved_sources()
                    .iter()
                    .enumerate()
                    .map(|(i, s)| SourceMenuEntry {
                        label: format!("{} '{}'", s.kind, s.display_name),
                        command: format!("source.saved.{i}"),
                        active: i as i32 == active,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut tree = menu_tree_with(&recent, &sources);
        // Flip the MCP label (the dynamic Start/Stop text; main.cpp:1568).
        let mcp_label = if self.mcp_running {
            "Stop MCP Server"
        } else {
            "Start MCP Server"
        };
        relabel_command(&mut tree, "tools.mcp", mcp_label);
        // Inject any enabled plugin-contributed menu commands into the &Plugins
        // submenu (design §6 Phase 2). With no contributing plugin loaded,
        // `ui_contributions()` is empty, so the tree is byte-identical to before —
        // the &Plugins submenu keeps only [Manage Plugins…].
        let plugin_commands = self.plugin_manager.ui_contributions();
        inject_plugin_menu_items(&mut tree, &plugin_commands);
        self.menubar.update(cx, |mb, cx| mb.set_menus(tree, cx));
        // After rebuilding the tree, re-push the active-source checkmark so the
        // saved-source row stays checked across the rebuild.
        let active_cmd = sources
            .iter()
            .position(|s| s.active)
            .map(|i| format!("source.saved.{i}"));
        if let Some(cmd) = active_cmd {
            self.menubar
                .update(cx, |mb, cx| mb.set_command_checked(&cmd, true, cx));
        }
        #[cfg(windows)]
        self.sync_console_menu_checked(cx);
    }
}
