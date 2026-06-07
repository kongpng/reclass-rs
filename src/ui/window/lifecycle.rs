//! App-lifecycle cluster — the unsaved-changes guard + quit/close flow, dirty-doc
//! collection + saving, the generic modal/confirm presenters, window-title
//! computation, dirty-state sync, and the toast `notify` helper. Extracted from
//! window.rs as an `impl super::MainWindow` block; `use super::*` inherits the
//! parent's imports.

use super::*;

impl super::MainWindow {
    // ── Unsaved-changes guard + quit (the C++ closeEvent + project_close) ──

    /// File ▸ Exit — if any open document is modified, show the 3-way
    /// unsaved-changes guard (Save changes / Discard / Cancel) before quitting
    /// (the C++ `closeEvent`; main.cpp:8984): **Save** persists each dirty
    /// document and aborts the quit on the first save failure, **Discard** quits
    /// without saving, **Cancel** aborts. With nothing dirty, quit immediately.
    pub(super) fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dirty = self.collect_dirty_docs(cx);
        if dirty.is_empty() {
            cx.quit();
            return;
        }
        self.open_unsaved_guard(
            dirty,
            |me, window, cx| {
                // Save succeeded for every dirty doc → quit.
                let _ = (me, window);
                cx.quit();
            },
            |_me, _window, cx| cx.quit(), // Discard → quit without saving.
            window,
            cx,
        );
    }

    /// The single window-close guard (the C++ `MainWindow::closeEvent`;
    /// main.cpp:8984-9031). Intercepts the OS/WM close (Alt+F4, `WM_DELETE_WINDOW`)
    /// and the in-app titlebar X, mirroring Qt's accept/ignore protocol:
    ///
    /// - **return `true`** ⇒ allow the close (the C++ `event->accept()`),
    /// - **return `false`** ⇒ abort *this* close (the C++ `event->ignore()`).
    ///
    /// With nothing dirty it returns `true` immediately (main.cpp:8998). Otherwise
    /// it opens the **async** 3-way unsaved-changes guard and returns `false`
    /// (abort) — the dialog is asynchronous, so the chosen Save/Discard branch
    /// performs the *actual* close itself ([`Window::remove_window`], the gpui
    /// analog of `event->accept()` for this single-window app). **Cancel** keeps the
    /// window open (no re-close), matching the C++ `event->ignore()` on Cancel
    /// (main.cpp:9013-9015).
    ///
    /// The programmatic re-close from the Save/Discard branch fires this hook
    /// again; the [`closing`](Self::closing) re-entrancy guard (the C++
    /// `ClosingGuard m_closingAll`; app-shell.md:167) makes that second pass return
    /// `true` without re-prompting.
    pub(super) fn guarded_window_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // Re-entrant close from the Save/Discard branch below → allow it through
        // without re-prompting (the C++ closing-all guard).
        if self.closing {
            return true;
        }
        let dirty = self.collect_dirty_docs(cx);
        if dirty.is_empty() {
            // Clean document set → accept the close (main.cpp:8998).
            return true;
        }
        self.open_unsaved_guard(
            dirty,
            |me, window, cx| {
                // Save succeeded for every dirty doc → perform the close.
                me.closing = true;
                window.remove_window();
                let _ = cx;
            },
            |me, window, cx| {
                // Discard → close without saving.
                me.closing = true;
                window.remove_window();
                let _ = cx;
            },
            window,
            cx,
        );
        // Abort *this* close — the async dialog's Save/Discard branch re-closes
        // (the C++ `event->ignore()` while the modal decides).
        false
    }

    /// Collect the open editors whose document is modified, deduped by document
    /// (the C++ `closeEvent` walks `m_tabs`, skipping repeat docs; here each tab
    /// owns its editor so we dedup by [`DocId`]). Each entry is
    /// `(doc id, editor, display name)` where the name is the file name when the
    /// document has a path, else the view-root struct name (the C++ name rule;
    /// main.cpp:8991-8993).
    pub(super) fn collect_dirty_docs(
        &self,
        cx: &Context<Self>,
    ) -> Vec<(Entity<crate::ui::editor::RcxEditor>, String)> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for t in self.document_area.read(cx).tabs() {
            if seen.contains(&t.id) {
                continue;
            }
            let ed = t.editor.read(cx);
            let ctrl = ed.controller();
            let doc = ctrl.document();
            let Some(name) = dirty_doc_name(
                doc.modified,
                doc.file_path.as_deref(),
                ctrl.tree(),
                ctrl.view_root_id(),
            ) else {
                continue;
            };
            seen.insert(t.id);
            out.push((t.editor.clone(), name));
        }
        out
    }

    /// Float `dialog` as a centered modal card of `width` × `margin_top` (the shared
    /// tail every modal opener repeated): `window.open_dialog` with no close button,
    /// optionally focus `focus`, then re-render. Collapses 12 copy-pasted tails.
    pub(super) fn present_modal<D: Render>(
        &self,
        dialog: &Entity<D>,
        width: f32,
        margin_top: f32,
        focus: Option<&FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let card = dialog.clone();
        window.open_dialog(cx, move |d, _window, _cx| {
            d.w(px(width))
                .margin_top(px(margin_top))
                .close_button(false)
                .child(card.clone())
        });
        if let Some(f) = focus {
            window.focus(f, cx);
        }
        cx.notify();
    }

    /// Open the 3-way unsaved-changes guard for `dirty` docs (the C++
    /// `ThemedMessageBox::unsavedChanges`). On **Save changes** it persists every
    /// dirty doc through its editor (the C++ `project_save(dock,false)` per doc),
    /// and only runs `on_saved` when ALL saved; on the first failure it reports +
    /// aborts (the C++ `event->ignore()` on a failed save). On **Discard** it runs
    /// `on_discard`. On **Cancel** (or Esc) it dismisses with no action.
    pub(super) fn open_unsaved_guard<S, D>(
        &mut self,
        dirty: Vec<(Entity<crate::ui::editor::RcxEditor>, String)>,
        on_saved: S,
        on_discard: D,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) where
        S: Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        D: Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    {
        let names = unique_dirty_names(dirty.iter().map(|(_, n)| n.clone()));
        let text = unsaved_changes_text(names.len());
        let editors: Vec<Entity<crate::ui::editor::RcxEditor>> =
            dirty.iter().map(|(e, _)| e.clone()).collect();
        let dialog = cx.new(|cx| RcxUnsavedDialog::new("Unsaved Changes", &text, names, cx));
        let focus = dialog.read(cx).focus_handle(cx);
        let on_saved = Rc::new(on_saved);
        let on_discard = Rc::new(on_discard);
        self.goto_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            move |this, _d, choice: &crate::ui::dialogs::messagebox::UnsavedChoice, window, cx| {
                use crate::ui::dialogs::messagebox::UnsavedChoice;
                match choice {
                    UnsavedChoice::Cancel => window.close_dialog(cx),
                    UnsavedChoice::Discard => {
                        window.close_dialog(cx);
                        on_discard(this, window, cx);
                    }
                    UnsavedChoice::Save => {
                        // Persist each dirty doc; abort on the first failure (the
                        // C++ `if (!project_save(...)) { event->ignore(); return; }`).
                        let all_saved = this.save_dirty_docs(&editors, window, cx);
                        if all_saved {
                            window.close_dialog(cx);
                            on_saved(this, window, cx);
                        }
                        // On failure the dialog stays open + a notification was
                        // raised, mirroring the C++ aborted close.
                    }
                }
            },
        ));
        self.present_modal(&dialog, crate::ui::dialogs::messagebox::MSG_MAX_WIDTH, 80., Some(&focus), window, cx);
    }

    /// Open a two-button confirm whose DEFAULT button is honoured — Enter triggers
    /// the spec's `default` (Cancel for destructive confirms, so a stray Enter can't
    /// destroy work), Esc cancels, and `on_accept` runs only on an explicit accept.
    /// Used in place of [`messagebox::open_confirm`](crate::ui::dialogs::messagebox) (the
    /// gpui-component `AlertDialog`) for destructive confirms, which it cannot make
    /// safe (no per-button focus hook; Enter is hard-bound to OK).
    pub(super) fn open_confirm_dialog<F>(
        &mut self,
        spec: crate::ui::dialogs::messagebox::MessageSpec,
        on_accept: F,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) where
        F: Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    {
        let dialog = cx.new(|cx| RcxConfirmDialog::new(spec, cx));
        let focus = dialog.read(cx).focus_handle(cx);
        let on_accept = Rc::new(on_accept);
        // Reuse the shared modal-subscription slot (a confirm and the unsaved guard
        // are never open at once).
        self.goto_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            move |this, _d, choice: &ConfirmChoice, window, cx| match choice {
                ConfirmChoice::Cancel => window.close_dialog(cx),
                ConfirmChoice::Accept => {
                    window.close_dialog(cx);
                    on_accept(this, window, cx);
                }
            },
        ));
        self.present_modal(&dialog, crate::ui::dialogs::messagebox::MSG_MAX_WIDTH, 80., Some(&focus), window, cx);
    }

    /// Persist each editor's document to its known path (the C++
    /// `project_save(dock,false)` per dirty doc). Returns `true` only if every doc
    /// was written; a doc with NO file path can't be saved synchronously here, so
    /// it counts as a failure (a notification points the user at Save As). On a
    /// write failure it notifies + returns `false` (the C++ aborts the close).
    pub(super) fn save_dirty_docs(
        &mut self,
        editors: &[Entity<crate::ui::editor::RcxEditor>],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        for editor in editors {
            let path = editor.read(cx).controller().document().file_path.clone();
            let Some(path) = path else {
                self.notify(
                    "An unsaved document has no file yet — use File ▸ Save As… first.",
                    window,
                    cx,
                );
                return false;
            };
            let ok = editor.update(cx, |ed, _cx| ed.controller_mut().document_mut().save(&path));
            if !ok {
                self.notify(format!("Failed to save {}", path.display()), window, cx);
                return false;
            }
            self.record_recent_file(&path, cx);
        }
        // Reflect cleared dirty bits in the tab dots.
        self.sync_dirty_state(cx);
        true
    }

    /// Recompute the OS window title from the active document and push it to the
    /// platform window (the C++ `updateWindowTitle`; main.cpp:5172). The title is
    /// `"<rootName>[ *] - Reclass"` where `<rootName>` is the struct name of the
    /// active tab's **view root** and the trailing `" *"` marks an unsaved
    /// document; with no active document it is plain `"Reclass"`. Call on every
    /// active-tab change, view-root change, and dirty-state change.
    pub(super) fn update_window_title(&self, window: &mut Window, cx: &Context<Self>) {
        let title = self.compute_window_title(cx);
        window.set_window_title(&title);
    }

    /// The window-title string for the active document (the pure half of
    /// [`update_window_title`], so it can be unit-tested via
    /// [`window_title_string`]). Reads the active editor's controller (tree +
    /// view root + dirty bit); falls back to `"Reclass"` when no document is open.
    pub(super) fn compute_window_title(&self, cx: &Context<Self>) -> String {
        let Some(editor) = self.document_area.read(cx).active_editor() else {
            return "Reclass".to_string();
        };
        let ed = editor.read(cx);
        let ctrl = ed.controller();
        let name = root_name_for_title(ctrl.tree(), ctrl.view_root_id());
        let modified = ctrl.document().modified;
        window_title_string(&name, modified)
    }

    /// Push the active document's modified state into its tab's dirty dot (the
    /// bug: the controller tracks `doc.modified` but the window never propagated
    /// it). Mirrors the C++ tab-title dirty marker.
    pub(super) fn sync_dirty_state(&mut self, cx: &mut Context<Self>) {
        let updates: Vec<(DocId, bool)> = self
            .document_area
            .read(cx)
            .tabs()
            .iter()
            .map(|t| (t.id, t.editor.read(cx).controller().document().modified))
            .collect();
        self.document_area.update(cx, |area, cx| {
            for (id, modified) in updates {
                area.set_modified(id, modified, cx);
            }
        });
    }

    /// Show a transient notification (the gpui-component notification layer; the
    /// graceful fallback for commands whose deeper workflow has no logic yet, and
    /// the "copied / exported / done" confirmations).
    pub(super) fn notify(
        &self,
        message: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.push_notification(Notification::info(message), cx);
    }
}
