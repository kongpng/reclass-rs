//! Workspace + document-area event handling (app-shell §8/§10) — the document-tab
//! events, editor observation, doc activation, workspace-panel navigation +
//! type-actions, opening a referenced type in a new tab, and rebuilding the
//! workspace model (including the cross-document Type Selector snapshot). Extracted
//! from window.rs as an `impl super::MainWindow` block; `use super::*` inherits the
//! parent's imports.

use super::*;

impl super::MainWindow {
    // ── Document-area event handling (app-shell §8 step 9) ──

    pub(super) fn on_doc_area_event(&mut self, ev: DocAreaEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            DocAreaEvent::Activated(id) => {
                // Mirror the activation into AppState if the id is known; the area
                // and state allocate ids independently but in lockstep order.
                self.activate_doc(id);
                // Re-feed the docks from the newly-active document's provider +
                // bookmarks and rebuild the dynamic menus (Data Source rows).
                self.refresh_docks_for_active(cx);
                self.rebuild_menus(cx);
            }
            DocAreaEvent::NewDocumentRequested => {
                // The center opened a fresh "Untitled" tab — the "+" sentinel,
                // closing the last tab, or Close-All. `push_document` gave it an
                // EMPTY editor (zero nodes → zero rows → every key/mouse handler
                // early-returns on `count == 0`), so the gutter was inert: no
                // expand, no Down, no right-click. Seed it with a root class —
                // identical to File ▸ New Class — so it is a real, interactive
                // document (the C++ "never leave a blank window" opens a fresh
                // struct, not a dead blank). Guarded on `is_empty` so a path that
                // later drives in a loaded doc is never clobbered.
                if let Some(editor) = self.document_area.read(cx).active_editor().cloned() {
                    if editor.read(cx).controller().tree().nodes.is_empty() {
                        editor.update(cx, |ed, cx| {
                            ed.set_document(seed_root_doc(RootKind::Class), cx);
                        });
                        self.apply_view_opts_to_editor(&editor, cx);
                        // Focus so arrow-nav / F2 / expand work immediately on the
                        // fresh tab (mirrors `new_document`'s deferred focus).
                        let handle = editor.read(cx).focus_handle(cx);
                        window.defer(cx, move |window, cx| window.focus(&handle, cx));
                    }
                }
                self.state.open_document("Untitled");
                self.rebuild_workspace(cx);
                // The tab set grew — re-observe so the new editor's selections
                // refresh the status bar.
                self.observe_editors(window, cx);
            }
            DocAreaEvent::Closed(id) => {
                self.state.close_document(id);
                self.rebuild_workspace(cx);
                self.observe_editors(window, cx);
            }
            DocAreaEvent::ViewModeChanged(id, mode) => {
                self.state.set_view_mode(id, mode);
            }
            DocAreaEvent::CodeOptionsChanged {
                format_idx,
                scope_idx,
            } => {
                // Persist the selector indices (the C++ `fmtCombo`/`scopeCombo`
                // `currentIndexChanged` → `setValue("codeFormat"/"codeScope")`;
                // main.cpp:2460/2469).
                let mut s = self.settings.borrow_mut();
                s.set(settings_keys::CODE_FORMAT, &format_idx.to_string());
                s.set(settings_keys::CODE_SCOPE, &scope_idx.to_string());
            }
        }
        cx.notify();
    }

    /// (Re)subscribe an observation to every open editor so a row selection
    /// re-renders the window — which re-runs [`StatusInfo::for_controller`] and
    /// repaints the bottom status bar with the live `Root.field  +0xNN` readout.
    ///
    /// The editors own the selection and `cx.notify()` themselves on a click but
    /// do not emit up to the window; an `observe` bridges that so a nested-entity
    /// change forces our (sibling) status bar to refresh. Called on construction
    /// and whenever the tab set changes (new/closed document); the previous
    /// observations are dropped (and thus unsubscribed) by reassigning the `Vec`.
    pub(super) fn observe_editors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editors: Vec<Entity<crate::ui::editor::RcxEditor>> = self
            .document_area
            .read(cx)
            .tabs()
            .iter()
            .map(|t| t.editor.clone())
            .collect();
        let mut subs = Vec::with_capacity(editors.len() * 2);
        for editor in editors {
            // A row-selection change re-renders the window (refreshes the status
            // bar). The editors `cx.notify()` themselves but don't emit up.
            subs.push(cx.observe(&editor, |_this, _editor, cx| cx.notify()));
            // Editor → host events. Ctrl+Click on a navigable header token emits
            // `OpenTypeInNewTab { ref_id }`; route it to the createTab+setViewRootId
            // flow (item 5). `Status` messages are harmless if unconsumed.
            subs.push(cx.subscribe_in(
                &editor,
                window,
                |this, _editor, ev: &crate::ui::editor::RcxEditorEvent, window, cx| match ev {
                    crate::ui::editor::RcxEditorEvent::OpenTypeInNewTab { ref_id } => {
                        this.open_type_in_new_tab(*ref_id, window, cx);
                    }
                    crate::ui::editor::RcxEditorEvent::Status { message } => {
                        this.notify(message.clone(), window, cx);
                    }
                    // The editor mutated its document (rename / structural op /
                    // undo). Rebuild the workspace TYPES list so a class rename /
                    // add / remove shows up in the left panel — the per-selection
                    // `observe` above only re-renders the status bar, leaving the
                    // cached `WorkspaceModel` stale (a renamed class kept its name).
                    crate::ui::editor::RcxEditorEvent::DocumentEdited => {
                        this.rebuild_workspace(cx);
                    }
                    // An in-editor View-option toggle (offset-margin double-click /
                    // right-click Relative/Absolute): the editor already applied it
                    // locally; mirror it to the window (persist + ✓ + push to every
                    // pane) so it behaves like the menu toggle (the C++
                    // `relativeOffsetsChanged`).
                    crate::ui::editor::RcxEditorEvent::ViewOptionToggled { option, value } => {
                        let opt = match option {
                            crate::ui::editor::EditorViewOption::RelativeOffsets => {
                                ViewOpt::RelativeOffsets
                            }
                        };
                        this.set_view_option_value(opt, *value, cx);
                    }
                    // Editor-owned keyboard popup (type / enum / source pickers):
                    // render it in OUR centered-modal overlay (no dialog focus_trap)
                    // and focus its input. The editor keeps its own outcome
                    // subscription; on Chosen/Cancel it emits `CloseModal` back.
                    crate::ui::editor::RcxEditorEvent::OpenModal {
                        view,
                        focus,
                        width,
                    } => {
                        this.open_centered_modal(
                            view.clone(),
                            focus.clone(),
                            *width,
                            window,
                            cx,
                        );
                    }
                    crate::ui::editor::RcxEditorEvent::CloseModal => {
                        this.close_top_modal(window, cx);
                    }
                },
            ));
        }
        self.editor_observers = subs;
    }

    /// Activate a document in `AppState` by id (best-effort: the document area is
    /// the source of truth for ordering).
    pub(super) fn activate_doc(&mut self, id: DocId) {
        // If AppState knows this id, activate it directly; else fall back to the
        // matching index so the two stay aligned.
        if self.state.tab(id).is_some() {
            self.state.activate(id);
        }
    }

    // ── Workspace quick navigation (app-shell §10) ──

    pub(super) fn on_workspace_nav(
        &mut self,
        nav: WorkspaceNav,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The workspace lists types from EVERY open tab, so the row may belong to a
        // document other than the active one. Switch to its owning tab FIRST (a
        // no-op when already active), then operate on that now-active editor — else
        // we'd re-root whatever tab happened to be active and the node lookup would
        // miss (the C++ workspace activates the target's dock before raising it).
        if !self
            .document_area
            .update(cx, |area, cx| area.activate_id(nav.doc, cx))
        {
            return; // stale row — its document is no longer open
        }
        // The C++ workspace double-click (main.cpp:6914): a node WITH a parent (a
        // field / nested member) navigates WITHIN its owner — set the view root to
        // the PARENT and scroll the field into view; a top-level type sets the view
        // root to the type itself ("Open in Current Tab": setViewRootId + raise).
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            return;
        };
        let parent_id = {
            let ed = editor.read(cx);
            let tree = ed.controller().tree();
            let idx = tree.index_of_id(nav.node_id);
            if idx >= 0 {
                tree.nodes[idx as usize].parent_id
            } else {
                0
            }
        };
        let view_root = if parent_id != 0 { parent_id } else { nav.node_id };
        editor.update(cx, |ed, cx| {
            ed.controller_mut().set_view_root_id(view_root);
            // recompose_view, NOT apply_document: navigation must not emit
            // DocumentEdited (which rebuilds the workspace and collapses its tree).
            ed.recompose_view(cx);
            if parent_id != 0 {
                ed.scroll_to_node_id(nav.node_id, cx);
            }
        });
        if parent_id != 0 {
            // Focus the editor so the just-selected field takes the caret and Enter /
            // F2 edit it immediately (the user's "press enter to edit the value after"),
            // rather than leaving keyboard focus in the workspace tree. Deferred so the
            // focus lands AFTER this navigation's recompose re-render.
            let focus = editor.read(cx).focus_handle(cx);
            window.defer(cx, move |window, cx| window.focus(&focus, cx));
        }
        if let Some(t) = self.state.active_tab_mut() {
            t.view_root = Some(view_root);
        }
        cx.notify();
    }

    /// Ctrl+Click on a navigable header token → open the referenced struct
    /// (`ref_id`) in a NEW tab (the C++ Ctrl+Click → `openTypeInNewTabRequested`
    /// → `createTab(doc)` + `setViewRootId`; main.cpp:3092). The Rust port's tabs
    /// each own their document, so "the same document in a new tab" is a deep copy
    /// of the active document's tree (node ids — and thus `ref_id` — preserved by
    /// [`NodeTree::clone`]) sharing the same provider `Arc`; the new tab's view
    /// root is set to `ref_id`. Without this the editor's
    /// [`RcxEditorEvent::OpenTypeInNewTab`] had no subscriber and the new tab was
    /// never created (item 5).
    pub(super) fn open_type_in_new_tab(&mut self, ref_id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.document_area.read(cx).active_editor().cloned() else {
            return;
        };
        // Deep-copy the active document (tree clone keeps ids/ref_ids; the provider
        // Arc is shared so the new tab reads the same source).
        let (tree, provider, file_path) = {
            let ed = active.read(cx);
            let doc = ed.controller().document();
            (
                doc.tree.clone(),
                doc.provider.clone(),
                doc.file_path.clone(),
            )
        };
        // Only open if the referenced struct actually exists in the tree (the C++
        // guards on a resolvable target before emitting).
        if tree.index_of_id(ref_id) < 0 {
            self.notify("No type to open in a new tab.", window, cx);
            return;
        }
        let mut doc = crate::controller::RcxDocument::new();
        doc.tree = tree;
        doc.provider = provider;
        doc.file_path = file_path;
        let title = root_name_for_title(&doc.tree, ref_id);
        let title = if title.is_empty() {
            "Untitled".to_string()
        } else {
            title
        };
        let source = Self::source_for_doc(&doc);
        // Append the new tab + push the cloned document into its editor, then set
        // the view root to the referenced struct (the C++ createTab + setViewRootId).
        let mut new_editor: Option<Entity<crate::ui::editor::RcxEditor>> = None;
        self.document_area.update(cx, |area, cx| {
            area.push_document(title.clone(), window, cx);
            if let Some(editor) = area.active_editor().cloned() {
                editor.update(cx, |ed, cx| {
                    ed.set_document(doc, cx);
                    ed.controller_mut().set_view_root_id(ref_id);
                    ed.apply_document(cx);
                });
                new_editor = Some(editor);
            }
        });
        if let Some(id) = self.active_doc_id(cx) {
            self.document_area.update(cx, |area, cx| {
                area.set_source(id, source.clone(), cx);
            });
        }
        // Mirror into AppState (title + source + view root) so the new tab is a
        // first-class document.
        let state_id = self.state.open_document(title);
        if let Some(t) = self.state.tab_mut(state_id) {
            t.view_root = Some(ref_id);
            t.source = source;
        }
        // Realign the fresh editor to the persisted view options + font, refresh
        // the docks + workspace, and re-observe so its events route.
        if let Some(editor) = new_editor {
            self.apply_view_opts_to_editor(&editor, cx);
        }
        self.rebuild_workspace(cx);
        self.refresh_docks_for_active(cx);
        self.observe_editors(window, cx);
        cx.notify();
    }

    /// The editor entity owning document `doc`, if it is open in a tab.
    pub(super) fn editor_for_doc(&self, doc: DocId, cx: &App) -> Option<Entity<crate::ui::editor::RcxEditor>> {
        self.document_area
            .read(cx)
            .tabs()
            .iter()
            .find(|t| t.id == doc)
            .map(|t| t.editor.clone())
    }

    /// Resolve a workspace type-row mutation (the C++ workspace `QMenu`:
    /// Rename / Duplicate / Delete / Add Member) against the owning document's
    /// live controller. The targeted node is addressed by `(doc, node_id)`; map
    /// the id to its tree index and drive the matching controller command, then
    /// recompose the editor, rebuild the workspace tree, and sync dirty state.
    pub(super) fn on_workspace_type_action(
        &mut self,
        action: WorkspaceTypeAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            WorkspaceTypeAction::Rename {
                doc,
                node_id,
                current,
            } => {
                // The C++ `renameType` opens `QInputDialog::getText`; collect the
                // new name in a free-text prompt, then apply on accept.
                let Some(editor) = self.editor_for_doc(doc, cx) else {
                    return;
                };
                let this = cx.entity().downgrade();
                self.open_text_prompt(
                    "Rename Type",
                    "New name",
                    &current,
                    window,
                    cx,
                    move |name, window, app| {
                        let name = name.trim().to_string();
                        if name.is_empty() {
                            return;
                        }
                        let _ = this.update(app, |me, cx| {
                            editor.update(cx, |ed, cx| {
                                let idx = ed.controller().tree().index_of_id(node_id);
                                if idx >= 0 {
                                    let node = &ed.controller().tree().nodes[idx as usize];
                                    // A top-level composite (enum/class/struct) is shown
                                    // by its struct_type_name — the C++ renameType renames
                                    // the TYPE; a child field is shown by its `name`.
                                    // Match the displayed name, else renaming a type wrote
                                    // the hidden instance `name` and looked like a no-op.
                                    let is_type =
                                        node.parent_id == 0 && !node.struct_type_name.is_empty();
                                    if is_type {
                                        ed.controller_mut().rename_struct_type(node_id, &name);
                                    } else {
                                        ed.controller_mut().rename_node(idx as usize, &name);
                                    }
                                    ed.apply_document(cx);
                                }
                            });
                            me.rebuild_workspace(cx);
                            me.sync_dirty_state(cx);
                            me.notify(format!("Renamed to {name}"), window, cx);
                            cx.notify();
                        });
                    },
                );
            }
            WorkspaceTypeAction::Duplicate { doc, node_id } => {
                let Some(editor) = self.editor_for_doc(doc, cx) else {
                    return;
                };
                editor.update(cx, |ed, cx| {
                    let idx = ed.controller().tree().index_of_id(node_id);
                    if idx >= 0 {
                        ed.controller_mut().duplicate_node(idx as usize);
                        ed.apply_document(cx);
                    }
                });
                self.rebuild_workspace(cx);
                self.sync_dirty_state(cx);
                self.notify("Duplicated type", window, cx);
                cx.notify();
            }
            WorkspaceTypeAction::Delete { doc, node_id } => {
                // Confirm before a destructive delete (the C++ asks before
                // deleting a top-level type).
                let Some(editor) = self.editor_for_doc(doc, cx) else {
                    return;
                };
                let name = editor.read(cx).controller().tree().index_of_id(node_id);
                if name < 0 {
                    return;
                }
                let spec = crate::ui::dialogs::messagebox::confirm(
                    "Delete Type",
                    "Delete this type and its members? This can be undone.",
                    "Delete",
                    true,
                );
                // Destructive: route through the hand-built confirm so Enter defaults
                // to Cancel (the AlertDialog would fire Delete on Enter).
                self.open_confirm_dialog(
                    spec,
                    move |me, window, cx| {
                        editor.update(cx, |ed, cx| {
                            let idx = ed.controller().tree().index_of_id(node_id);
                            if idx < 0 {
                                return;
                            }
                            let node = ed.controller().tree().nodes[idx as usize].clone();
                            // A top-level struct uses `deleteRootStruct` (it also
                            // rebinds refs + the view root); a member field uses
                            // `removeNode`.
                            if node.parent_id == 0 && node.kind == crate::core::NodeKind::Struct {
                                ed.controller_mut().delete_root_struct(node_id);
                            } else {
                                ed.controller_mut().remove_node(idx as usize);
                            }
                            ed.apply_document(cx);
                        });
                        me.rebuild_workspace(cx);
                        me.sync_dirty_state(cx);
                        me.notify("Deleted type", window, cx);
                        cx.notify();
                    },
                    window,
                    cx,
                );
            }
            WorkspaceTypeAction::TogglePin { doc: _, node_id } => {
                // Toggle membership in the pinned set + rebuild so the PINNED section
                // (re)appears (the C++ m_pinnedIds toggle + rebuild, main.cpp:6858).
                if !self.pinned_ids.remove(&node_id) {
                    self.pinned_ids.insert(node_id);
                }
                self.rebuild_workspace(cx);
            }
            WorkspaceTypeAction::AddMember { doc, node_id } => {
                let Some(editor) = self.editor_for_doc(doc, cx) else {
                    return;
                };
                editor.update(cx, |ed, cx| {
                    let idx = ed.controller().tree().index_of_id(node_id);
                    if idx < 0 {
                        return;
                    }
                    let node = ed.controller().tree().nodes[idx as usize].clone();
                    // Append a new member at the end of the struct. For a struct row
                    // the members are its children; for a field row, add a sibling
                    // into the field's parent.
                    let parent_id = if node.kind == crate::core::NodeKind::Struct {
                        node.id
                    } else {
                        node.parent_id
                    };
                    ed.controller_mut().insert_node(
                        parent_id,
                        -1,
                        crate::core::NodeKind::Hex64,
                        "new_member",
                    );
                    ed.apply_document(cx);
                });
                self.rebuild_workspace(cx);
                self.sync_dirty_state(cx);
                self.notify("Added member", window, cx);
                cx.notify();
            }
            WorkspaceTypeAction::ChangeType { doc, node_id } => {
                // Bring the field into its editor and open the gutter Type Selector
                // on it. The picker is a window-centered modal (`OpenModal` →
                // `open_centered_modal`), so it shows regardless of the active tab;
                // `reveal_and_change_type` re-roots via `recompose_view` (no
                // DocumentEdited, so the workspace tree is not collapsed).
                let Some(editor) = self.editor_for_doc(doc, cx) else {
                    return;
                };
                editor.update(cx, |ed, cx| {
                    ed.reveal_and_change_type(node_id, window, cx);
                });
                cx.notify();
            }
            WorkspaceTypeAction::Comment { doc, node_id } => {
                // Edit the field's comment via a text prompt seeded from the node's
                // current comment (the model carries none). Empty input clears it.
                let Some(editor) = self.editor_for_doc(doc, cx) else {
                    return;
                };
                let current = {
                    let ed = editor.read(cx);
                    let tree = ed.controller().tree();
                    let idx = tree.index_of_id(node_id);
                    if idx >= 0 {
                        tree.nodes[idx as usize].comment.clone()
                    } else {
                        String::new()
                    }
                };
                let this = cx.entity().downgrade();
                self.open_text_prompt(
                    "Edit Comment",
                    "Comment",
                    &current,
                    window,
                    cx,
                    move |text, window, app| {
                        let comment = text.trim().to_string();
                        let _ = this.update(app, |me, cx| {
                            editor.update(cx, |ed, cx| {
                                ed.controller_mut().set_comment(node_id, &comment);
                                ed.apply_document(cx);
                            });
                            me.rebuild_workspace(cx);
                            me.sync_dirty_state(cx);
                            me.notify("Comment updated", window, cx);
                            cx.notify();
                        });
                    },
                );
            }
        }
    }

    // ── Workspace model rebuild (app-shell §10 `rebuildWorkspaceModel`) ──

    /// Rebuild the workspace tree model from the open documents' trees and push it
    /// to the [`WorkspacePanel`]. The editor controllers own the trees; here we
    /// borrow each one **by reference** (no clone — `NodeTree` is a logic type we
    /// don't extend) and build the model in one pass.
    pub(super) fn rebuild_workspace(&mut self, cx: &mut Context<Self>) {
        // Snapshot the per-tab editor handles + ids first (a short borrow of the
        // area), then build against live read-borrows of each controller's tree.
        let entries: Vec<(DocId, Entity<crate::ui::editor::RcxEditor>)> = self
            .document_area
            .read(cx)
            .tabs()
            .iter()
            .map(|t| (t.id, t.editor.clone()))
            .collect();

        let viewed: Vec<u64> = entries
            .iter()
            .map(|(_, ed)| ed.read(cx).controller().view_root_id())
            .filter(|&id| id != 0)
            .collect();

        // Hold the editor read-guards so their `&NodeTree`s stay valid for the
        // build call (all immutable borrows of `cx` coexist).
        let guards: Vec<(DocId, &crate::ui::editor::RcxEditor)> =
            entries.iter().map(|(id, ed)| (*id, ed.read(cx))).collect();
        let docs: Vec<WorkspaceDoc> = guards
            .iter()
            .map(|(id, ed)| WorkspaceDoc {
                doc: *id,
                tree: ed.controller().tree(),
            })
            .collect();
        // P5: snapshot every document's top-level structs while the read-guards are
        // still alive — the cross-document Type Selector composites (C++ m_projectDocs).
        let per_doc: Vec<(DocId, Vec<crate::ui::editor::CrossDocComposite>)> = guards
            .iter()
            .map(|(id, ed)| {
                (
                    *id,
                    crate::ui::editor::CrossDocComposite::top_level_in(ed.controller().tree()),
                )
            })
            .collect();
        let pins: Vec<u64> = self.pinned_ids.iter().copied().collect();
        let model = WorkspaceModel::build(&docs, &pins, &viewed);

        // The immutable editor borrows (guards/docs) end at the build above (NLL),
        // so we can now hand each editor the union of every OTHER document's
        // top-level structs (deduped by name) for its Type Selector catalogue.
        for (id, ed) in &entries {
            let mut others: Vec<crate::ui::editor::CrossDocComposite> = Vec::new();
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            for (other_id, comps) in &per_doc {
                if other_id == id {
                    continue;
                }
                for c in comps {
                    if seen.insert(c.name.clone()) {
                        others.push(c.clone());
                    }
                }
            }
            ed.update(cx, |e, _cx| e.set_cross_doc_composites(others));
        }

        self.workspace.update(cx, |ws, cx| {
            ws.set_model(model, cx);
        });
    }
}
