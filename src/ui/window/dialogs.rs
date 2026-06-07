//! Tools / Help dialog cluster — the RTTI browser, performance profiler, MCP
//! toggle, type-aliases editor, options dialog (+ apply_options), theme editor,
//! About, and Shortcuts. Extracted from window.rs as an `impl super::MainWindow`
//! block; `use super::*` inherits the parent's imports.

use super::*;

/// The GitHub URL the Help ▸ About dialog advertises (the C++ About dialog's
/// "Open GitHub" button opens `https://github.com/IChooseYou/Reclass`;
/// main.cpp:4434). Was wrongly `github.com/reclassnet/reclass` (item 8).
pub(crate) const ABOUT_GITHUB_URL: &str = "https://github.com/IChooseYou/Reclass";

impl super::MainWindow {
    // ── Tools / Help ──

    /// Tools ▸ RTTI Browser (Ctrl+Shift+R) — open the vtable/RTTI hierarchy browser
    /// for the user's single selected hex/pointer field. Faithful port of the
    /// Tools-menu gate (`main.cpp:1523-1561`) + `MainWindow::showRttiBrowser`
    /// (`main.cpp:4395-4413`):
    ///
    /// 1. Resolve the active editor → its controller → tree + provider.
    /// 2. The Tools-menu gate ([`resolve_field_vtable`]) masks the single selected
    ///    id, requires a Hex32/64 or Pointer32/64 word, computes its absolute
    ///    address, reads the stored word and rejects null — surfacing each
    ///    rejection as the C++ `setAppStatus(...)` string.
    /// 3. Walk RTTI at the candidate vtable ([`resolve_rtti`], MSVC first then the
    ///    additive Itanium fallback); on failure show the walker error / the empty
    ///    placeholder (the C++ `ThemedMessageBox::info("No RTTI Here", …)`).
    /// 4. Otherwise open the [`RttiBrowserDialog`] modal (the C++ `dlg.exec()`).
    ///
    /// The C++ `showRttiBrowser` always walks with `ptrSize = 8` (the `walkRtti`
    /// default arg); mirror that with `max(tree.pointer_size, 8)`.
    pub(super) fn open_rtti_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::rtti::browser::{
            resolve_field_vtable, resolve_rtti, RttiBrowserDialog, RttiBrowserEvent,
        };

        let Some(editor) = self.active_editor_or_notify("Open a document first.", window, cx)
        else {
            return;
        };

        // Gate the selection against the live tree + provider, then walk RTTI.
        // Both steps are pure logic on borrows of the editor's controller, so do
        // them inside a single read borrow and surface the outcome afterwards.
        let outcome = {
            let ed = editor.read(cx);
            let ctrl = ed.controller();
            let tree = ctrl.tree();
            let ptr_size = tree.pointer_size.max(8);
            let sel: Vec<u64> = ctrl.selected_ids().iter().copied().collect();
            let prov = ctrl.provider().clone();
            match resolve_field_vtable(tree, &sel, prov.as_ref()) {
                Err(e) => Err(e.message().to_string()),
                Ok(vtable) => {
                    let info = resolve_rtti(prov.as_ref(), vtable, ptr_size, 64);
                    if !info.ok {
                        Err(if info.error.is_empty() {
                            format!("No RTTI structures found at 0x{vtable:x}.")
                        } else {
                            info.error.clone()
                        })
                    } else {
                        Ok(info)
                    }
                }
            }
        };

        let info = match outcome {
            Ok(info) => info,
            Err(msg) => {
                self.notify(msg, window, cx);
                return;
            }
        };

        let dlg = cx.new(|cx| RttiBrowserDialog::new(info, window, cx));
        let focus = dlg.read(cx).focus_handle(cx);
        self.goto_sub = Some(cx.subscribe_in(
            &dlg,
            window,
            |_w, _dlg, ev: &RttiBrowserEvent, window, cx| match ev {
                RttiBrowserEvent::Close => window.close_dialog(cx),
            },
        ));
        self.present_modal(&dlg, 720., 80., Some(&focus), window, cx);
    }

    /// Tools ▸ Performance Profiler (Ctrl+Shift+F) — open the live
    /// [`ProfilerDialog`] (the C++ `ProfilerDialog`; profilerdialog.cpp:106).
    /// The dialog auto-enables profiling on open, takes a live snapshot, and
    /// auto-refreshes at ~2 Hz while shown; it restores the prior profiling
    /// flag on close. Close-only, so it reuses the generic [`goto_sub`](Self::goto_sub)
    /// subscription slot like the other single-button dialogs.
    pub(super) fn open_profiler_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::ui::dialogs::{ProfilerDialog, ProfilerEvent};
        let dialog = cx.new(|cx| ProfilerDialog::new(window, cx));
        let focus = dialog.read(cx).focus_handle(cx);
        self.goto_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            |_this, _d, _ev: &ProfilerEvent, window, cx| {
                window.close_dialog(cx);
            },
        ));
        // The ProfilerDialog renders its own self-clamping card (820×640); the
        // outer overlay just hosts it (no extra card / close button).
        self.present_modal(&dialog, 820., 48., Some(&focus), window, cx);
    }

    /// Tools ▸ Start/Stop MCP Server — toggle the MCP bridge flag and flip the
    /// menu label (the C++ `toggleMcp` + dynamic action text; main.cpp:1568). The
    /// UI does not embed a live bridge (the MCP server runs via the `mcp` feature,
    /// not a platform-specific path); the toggle + label are real.
    pub(super) fn toggle_mcp(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mcp_running = !self.mcp_running;
        self.rebuild_menus(cx);
        let msg = if self.mcp_running {
            "MCP Server started."
        } else {
            "MCP Server stopped."
        };
        self.notify(msg, window, cx);
    }

    /// Tools ▸ Type Aliases… — open the [`TypeAliasesDialog`] seeded from the
    /// active document's per-kind alias map (the C++ `showTypeAliasesDialog`). On
    /// accept, apply the edited aliases to the document and recompose so the
    /// editor + generated code reflect the new type names.
    pub(super) fn open_type_aliases_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor_or_notify("Open a document first.", window, cx)
        else {
            return;
        };
        let current: std::collections::HashMap<crate::core::NodeKind, String> =
            editor.read(cx).controller().document().type_aliases.clone();
        let dlg = cx.new(|cx| TypeAliasesDialog::new(current, window, cx));
        let editor2 = editor.clone();
        let this = cx.entity().downgrade();
        self.goto_sub = Some(cx.subscribe_in(
            &dlg,
            window,
            move |_w, dlg, ev: &TypeAliasesEvent, window, cx| match ev {
                TypeAliasesEvent::Accept => {
                    let map = dlg.read(cx).collect(cx);
                    window.close_dialog(cx);
                    let _ = this.update(cx, |me, cx| {
                        editor2.update(cx, |ed, cx| {
                            ed.controller_mut().document_mut().type_aliases = map;
                            ed.apply_document(cx);
                        });
                        me.sync_dirty_state(cx);
                        me.notify("Type aliases updated", window, cx);
                        cx.notify();
                    });
                }
                TypeAliasesEvent::Cancel => window.close_dialog(cx),
            },
        ));
        let focus = dlg.read(cx).focus_handle(cx);
        self.present_modal(&dlg, 460., 80., Some(&focus), window, cx);
    }

    /// Tools ▸ Options — open the [`OptionsDialog`] seeded from the live window
    /// state, subscribe to its Apply event, and on accept apply each field to
    /// the live window/editors/controllers + persist via the disk store (the
    /// C++ `showOptionsDialog` → apply + `QSettings::setValue`).
    pub(super) fn open_options_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::ui::dialogs::optionsdialog::{OptionsDialog, OptionsEvent, OptionsResult};
        let themes: Vec<String> = self
            .theme_manager
            .borrow()
            .themes()
            .iter()
            .map(|t| t.name.clone())
            .collect();
        let theme_index = self.theme_manager.borrow().current_index();
        // Seed the dialog with the LIVE persisted state (the C++ builds `current`
        // from `m_menuBarTitleCase` / `showIcon` etc; main.cpp:5019). Previously
        // these two were hardcoded, so the dialog never reflected — and Apply could
        // never preserve — the user's real preference.
        let current = OptionsResult {
            theme_index,
            font_name: self.editor_font.clone(),
            menu_bar_title_case: self.menu_bar_title_case,
            show_icon: self.show_icon,
            auto_start_mcp: self.auto_start_mcp,
            refresh_ms: self.refresh_ms,
            generator_asserts: self.generator_asserts,
            brace_wrap: self.brace_wrap,
        };
        let dialog = cx.new(|cx| OptionsDialog::new(current, themes, window, cx));
        let focus = dialog.read(cx).focus_handle(cx);
        self.options_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            |this, _d, ev: &OptionsEvent, window, cx| match ev {
                OptionsEvent::Apply(result) => {
                    let result = result.clone();
                    window.close_dialog(cx);
                    this.apply_options(result, window, cx);
                }
                OptionsEvent::Cancel => window.close_dialog(cx),
            },
        ));
        self.present_modal(&dialog, 640., 80., Some(&focus), window, cx);
    }

    /// View ▸ Edit Theme… — open the dedicated [`ThemeEditor`] (the swatch grid /
    /// live preview / theme combo / name edit / save-as-user-copy) for the
    /// active theme (the C++ `editTheme`; main.cpp). On accept the editor has
    /// already committed via the manager (`updateTheme` — persists + commits the
    /// preview + re-styles), so the host only re-syncs its theme state + the
    /// Theme submenu ✓; on reject the editor reverts the live preview
    /// (`revertPreview`). Previously `view.theme_edit` routed to the Options
    /// dialog and this fully-built view had no caller (item 4).
    pub(super) fn open_theme_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::theme::editor::{ThemeEditor, ThemeEditorEvent};
        let index = self.theme_manager.borrow().current_index();
        let dialog = cx.new(|cx| ThemeEditor::new(index, window, cx));
        let focus = dialog.read(cx).focus_handle(cx);
        self.theme_editor_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            |this, _d, ev: &ThemeEditorEvent, window, cx| match ev {
                // Saved: the manager already committed + re-styled in
                // `ThemeEditor::save` (the C++ `updateTheme`); mirror the commit
                // into the host's theme state + Theme submenu ✓.
                ThemeEditorEvent::Saved(_index) => {
                    window.close_dialog(cx);
                    let name = this.theme_manager.borrow().current().name.clone();
                    this.state.set_theme_name(&name);
                    this.settings.borrow_mut().set(settings_keys::THEME, &name);
                    this.sync_theme_menu_checked(cx);
                    cx.notify();
                }
                // Cancelled: the editor already reverted the live preview (the
                // C++ `revertPreview`); just dismiss.
                ThemeEditorEvent::Cancelled => {
                    window.close_dialog(cx);
                    cx.notify();
                }
            },
        ));
        self.present_modal(&dialog, 480., 60., Some(&focus), window, cx);
    }

    /// Apply an accepted [`OptionsResult`] to the live app + persist every field
    /// (the C++ Options "OK" path). Theme + font reuse the existing switch/set
    /// helpers (which persist on their own); the remaining fields persist here.
    pub(super) fn apply_options(
        &mut self,
        result: crate::ui::dialogs::optionsdialog::OptionsResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Theme (the C++ `m_themeCombo` → `setCurrent`). `switch_theme` re-styles
        // + records the active name; persist it via the disk store under "theme".
        let theme_name = self
            .theme_manager
            .borrow()
            .themes()
            .get(result.theme_index)
            .map(|t| t.name.clone());
        if let Some(name) = theme_name {
            self.switch_theme(result.theme_index, window, cx);
            self.sync_theme_menu_checked(cx);
            self.settings.borrow_mut().set(settings_keys::THEME, &name);
        }
        // Font (reuses `set_editor_font`, which persists "font" + syncs the menu).
        if result.font_name != self.editor_font && !result.font_name.is_empty() {
            self.set_editor_font(&result.font_name.clone(), window, cx);
        }
        // Refresh interval — push into every controller + persist "refreshMs".
        self.refresh_ms = result.refresh_ms.clamp(
            crate::ui::dialogs::optionsdialog::REFRESH_MIN,
            crate::ui::dialogs::optionsdialog::REFRESH_MAX,
        );
        self.settings
            .borrow_mut()
            .set(settings_keys::REFRESH_MS, &self.refresh_ms.to_string());
        // Brace-wrap (generator) — push into every controller + persist.
        self.brace_wrap = result.brace_wrap;
        self.settings
            .borrow_mut()
            .set_bool(settings_keys::BRACE_WRAP, self.brace_wrap);
        self.generator_asserts = result.generator_asserts;
        self.settings
            .borrow_mut()
            .set_bool(settings_keys::GENERATOR_ASSERTS, self.generator_asserts);
        // Reflect the new assert flag into the code-view so the live rendered pane
        // updates without a tab switch (the C++ `refreshAllRendered`; main.cpp:2451).
        let emit_asserts = self.generator_asserts;
        self.document_area.update(cx, |area, cx| {
            area.set_generator_asserts(emit_asserts, cx);
        });
        // Push refresh + brace-wrap into every open controller.
        let refresh_ms = self.refresh_ms;
        let brace_wrap = self.brace_wrap;
        let editors: Vec<Entity<crate::ui::editor::RcxEditor>> = self
            .document_area
            .read(cx)
            .tabs()
            .iter()
            .map(|t| t.editor.clone())
            .collect();
        for editor in editors {
            editor.update(cx, |ed, _cx| {
                ed.controller_mut().set_refresh_interval(refresh_ms);
                ed.controller_mut().set_brace_wrap(brace_wrap);
            });
        }
        // MCP autostart — persist; reflect the label via the running flag.
        self.auto_start_mcp = result.auto_start_mcp;
        self.settings
            .borrow_mut()
            .set_bool(settings_keys::AUTO_START_MCP, self.auto_start_mcp);
        // Menu-bar title-case (the C++ `r.menuBarTitleCase` → `applyMenuBarTitleCase`
        // + persist "menuBarTitleCase"; main.cpp:5041). Push into the live menu bar
        // so the titles re-case immediately, then persist across launches.
        if result.menu_bar_title_case != self.menu_bar_title_case {
            self.menu_bar_title_case = result.menu_bar_title_case;
            let title_case = self.menu_bar_title_case;
            self.menubar
                .update(cx, |mb, cx| mb.set_title_case(title_case, cx));
            self.settings
                .borrow_mut()
                .set_bool(settings_keys::MENU_BAR_TITLE_CASE, self.menu_bar_title_case);
        }
        // Titlebar show-icon (the C++ `r.showIcon` → `m_titleBar->setShowIcon` +
        // persist "showIcon"; main.cpp:5046). When on, the titlebar swaps its bold
        // "Reclass" text for a class-icon badge (the C++ `setShowIcon(true)` clears
        // `m_appLabel`'s text and sets it to the class.png pixmap; titlebar.cpp:202-
        // 214). The flag re-renders the bar on the next paint (see the
        // `render_titlebar` call site, which passes `self.show_icon`).
        if result.show_icon != self.show_icon {
            self.show_icon = result.show_icon;
            self.settings
                .borrow_mut()
                .set_bool(settings_keys::SHOW_ICON, self.show_icon);
        }
        self.notify("Options applied.", window, cx);
        cx.notify();
    }

    /// Help ▸ About Reclass — a themed message box with build info + a note on the
    /// project (the C++ `about()` themed dialog; main.cpp:4415). The GitHub button
    /// is folded into the body text (the modal message box is single-button).
    pub(super) fn show_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let spec = crate::ui::dialogs::messagebox::info(
            "About Reclass",
            &format!(
                "Reclass {} — a Rust + GPUI port of ReClass.\n\nA memory structure editor.\n\
                 GitHub: {ABOUT_GITHUB_URL}",
                env!("CARGO_PKG_VERSION")
            ),
        );
        crate::ui::dialogs::messagebox::open_message(spec, window, cx);
    }

    /// Help ▸ Keyboard Shortcuts… (F1) — a themed reference of the bound
    /// accelerators (the C++ `showShortcutsDialog`; main.cpp:4440).
    pub(super) fn show_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let detail = vec![
            "Ctrl+N / Ctrl+T / Ctrl+E — New Class / Struct / Enum".to_string(),
            "Ctrl+O — Open    Ctrl+S — Save    Ctrl+Shift+S — Save As".to_string(),
            "Ctrl+W — Close    Ctrl+Z / Ctrl+Y — Undo / Redo".to_string(),
            "Ctrl+B — Add Bookmark    Ctrl+Alt+B — Quick Bookmark".to_string(),
            "F5 — Refresh    Ctrl+G — Go to Address".to_string(),
            "Ctrl+K / Ctrl+Shift+P / F1 — Command Palette / Shortcuts".to_string(),
            "Ctrl+Shift+M — Memory Scanner    Ctrl+Shift+Y — Modules".to_string(),
            "Ctrl+Shift+B — Bookmarks    Ctrl+\\ — Split Editor".to_string(),
        ];
        let mut spec =
            crate::ui::dialogs::messagebox::info("Keyboard Shortcuts", "Bound accelerators:");
        spec.detail = detail;
        crate::ui::dialogs::messagebox::open_message(spec, window, cx);
    }
}
