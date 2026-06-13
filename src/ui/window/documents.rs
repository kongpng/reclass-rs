//! Active-document operations cluster — new-document creation, data-source
//! selection (data file / process picker / clear), the Bookmarks panel ops, the
//! memory-Scanner event handlers, and the editor-font setting. Extracted from
//! window.rs as an `impl super::MainWindow` block; `use super::*` inherits the
//! parent's imports.

use super::*;

/// File ▸ New {Class / Struct / Enum} — the root kind a fresh document is seeded
/// with (the C++ `project_new(keyword)`; main.cpp:4047). Determines the seed
/// root's `class_keyword` and the tab title.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RootKind {
    Class,
    Struct,
    Enum,
}

impl RootKind {
    /// The C/C++ keyword stored on the seed root node ("class" / "struct" /
    /// "enum"); the compose pipeline renders it verbatim.
    pub(crate) fn class_keyword(self) -> &'static str {
        match self {
            RootKind::Class => "class",
            RootKind::Struct => "struct",
            RootKind::Enum => "enum",
        }
    }

    /// The new tab's title — distinct per kind so the three commands don't all
    /// land on an indistinguishable "Untitled".
    pub(crate) fn title(self) -> &'static str {
        match self {
            RootKind::Class => "Untitled Class",
            RootKind::Struct => "Untitled Struct",
            RootKind::Enum => "Untitled Enum",
        }
    }

    /// The seed root's type name.
    pub(crate) fn type_name(self) -> &'static str {
        match self {
            RootKind::Class => "NewClass",
            RootKind::Struct => "NewStruct",
            RootKind::Enum => "NewEnum",
        }
    }
}

/// Build a fresh document seeded with a single root struct of the given kind
/// (the C++ `project_new` template; main.cpp:6097 seeds a base address + a root
/// struct with 16 hex fields). The root carries the kind's `class_keyword` so
/// the rendered code reads `class` / `struct` / `enum`. Uses only the public
/// [`NodeTree`] API — no logic-module change.
pub(crate) fn seed_root_doc(kind: RootKind) -> crate::controller::RcxDocument {
    use crate::core::{Node, NodeKind};
    let mut doc = crate::controller::RcxDocument::new();
    // The C++ template lands a sensible default base so addresses read naturally.
    doc.tree.base_address = 0x0040_0000;
    let is32 = doc.tree.pointer_size < 8;
    let (hex_kind, stride) = if is32 {
        (NodeKind::Hex32, 4)
    } else {
        (NodeKind::Hex64, 8)
    };
    let mut root = Node {
        kind: NodeKind::Struct,
        name: "instance".to_string(),
        struct_type_name: kind.type_name().to_string(),
        class_keyword: kind.class_keyword().to_string(),
        parent_id: 0,
        offset: 0,
        ..Node::default()
    };
    root.id = doc.tree.reserve_id();
    let root_id = root.id;
    // File > New Enum seeds an enum root with 5 named members and NO hex fields
    // (the C++ `buildEmptyStruct` enum branch, main.cpp:3981-4000). Without this the
    // empty-members render gate (`is_enum() && !enum_members.is_empty()`) fails and
    // the 16 hex children would render as ordinary fields on screen and in exports.
    if matches!(kind, RootKind::Enum) {
        root.enum_members = (0..5).map(|i| (format!("Member{i}"), i as i64)).collect();
        doc.tree.add_node(root);
        doc.tree.touch();
        return doc;
    }
    doc.tree.add_node(root);
    for i in 0..16 {
        let mut c = Node {
            kind: hex_kind,
            name: format!("field_{:02x}", i * stride),
            parent_id: root_id,
            offset: i * stride,
            ..Node::default()
        };
        c.id = doc.tree.reserve_id();
        doc.tree.add_node(c);
    }
    doc.tree.touch();
    doc
}

impl super::MainWindow {
    /// Open a fresh document tab seeded with the chosen root kind (the C++
    /// `project_new(keyword)` family: `newClass` passes "class", `newStruct`
    /// none, `newEnum` "enum"; main.cpp:4047). The root node carries the matching
    /// `class_keyword` so the rendered C/C++ reads `class` / `struct` / `enum`,
    /// and the tab title reflects the kind so the three commands are visibly
    /// distinct (the bug: all three collapsed to one blank "Untitled").
    pub(super) fn new_document(
        &mut self,
        kind: RootKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_start_page(cx);

        // C++ `project_new` adds the new struct to the active document and copies its
        // saved sources (main.cpp:6058-6090, `copySavedSources`) so File ▸ New can
        // immediately read the same target. The Rust model keeps documents
        // independent, so instead INHERIT the active source onto the fresh doc:
        // capture it BEFORE the new tab becomes active, and only when one exists.
        let inherited = self.document_area.read(cx).active_editor().and_then(|ed| {
            let ed = ed.read(cx);
            let ctrl = ed.controller();
            (!ctrl.saved_sources().is_empty()).then(|| {
                (
                    ctrl.saved_sources().to_vec(),
                    ctrl.active_source_index(),
                    ctrl.provider().clone(),
                    Self::source_for_controller(ctrl),
                )
            })
        });

        let title = kind.title();
        let doc = seed_root_doc(kind);
        let mut new_editor: Option<Entity<crate::ui::editor::RcxEditor>> = None;
        self.document_area.update(cx, |area, cx| {
            area.push_document(title, window, cx);
            // Push the seeded tree into the just-created tab's editor.
            if let Some(editor) = area.active_editor().cloned() {
                editor.update(cx, |ed, cx| ed.set_document(doc, cx));
                new_editor = Some(editor);
            }
        });
        // Realign the fresh editor to the window's (persisted) view options so a
        // new tab honours the current compact-columns/tree-lines/etc. state.
        if let Some(editor) = &new_editor {
            self.apply_view_opts_to_editor(editor, cx);
            // Focus the fresh editor so the keyboard (arrow-nav, F2 rename, …) works
            // immediately without a click — the C++ `m_sci->setFocus()` on a new
            // tab. Deferred so the focus lands once the editor has mounted.
            let handle = editor.read(cx).focus_handle(cx);
            window.defer(cx, move |window, cx| window.focus(&handle, cx));
        }
        self.state.open_document(title);
        self.rebuild_workspace(cx);

        if let Some((sources, active_idx, provider, source)) = inherited {
            // Carry the saved-source list/index (copySavedSources) and SHARE the live
            // provider (an `Arc`), then recompose so the New Class reads the inherited
            // target. `set_active_source` re-feeds the docks + Data-Source menu + the
            // tab source icon for the now-active new document.
            if let Some(editor) = &new_editor {
                editor.update(cx, |ed, cx| {
                    ed.controller_mut().copy_saved_sources(sources, active_idx);
                    ed.controller_mut().attach_provider(provider, false);
                    ed.apply_document(cx);
                });
            }
            self.set_active_source(source, window, cx);
        } else {
            // A fresh doc has the NullProvider — clear the docks accordingly.
            self.refresh_docks_for_active(cx);
        }
        self.observe_editors(window, cx);
    }

    // ── File: data source providers (the C++ m_sourceMenu → selectSource) ──

    /// The active document's editor, or — when none is open — show `msg` and
    /// return None (the shared "Open a document first." guard).
    pub(super) fn active_editor_or_notify(
        &mut self,
        msg: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<crate::ui::editor::RcxEditor>> {
        match self.document_area.read(cx).active_editor().cloned() {
            Some(e) => Some(e),
            None => {
                self.notify(msg, window, cx);
                None
            }
        }
    }

    /// File ▸ Data Source ▸ File — attach a binary file as the active document's
    /// data source via the native file picker (the C++ `loadData(path)` /
    /// File-provider attach). Updates the tab source icon + window state.
    pub(super) fn prompt_data_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor_or_notify("Open a document first.", window, cx)
        else {
            return;
        };
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Attach a binary data file".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = rx
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten()
                .and_then(|v| v.into_iter().next())
            else {
                return;
            };
            let _ = this.update_in(cx, |me, window, cx| {
                editor.update(cx, |ed, _cx| {
                    // Route through the controller so the attach clears the undo
                    // stack and resets the live snapshot, matching C++
                    // `RcxDocument::loadData(path)` (controller.cpp:292). Calling
                    // `document_mut().load_data_file` directly would bypass both.
                    ed.controller_mut().attach_data_file(&path);
                });
                // Recompose against the freshly-attached provider + reflect the
                // File source icon in the tab and window state.
                editor.update(cx, |ed, cx| ed.apply_document(cx));
                let source = crate::ui::state::DataSource::new(
                    crate::ui::state::SourceKind::File,
                    path.to_string_lossy().into_owned(),
                );
                me.set_active_source(source, window, cx);
                me.notify(format!("Attached {}", path.display()), window, cx);
            });
        })
        .detach();
    }

    /// File ▸ Data Source ▸ Process Memory — attach through the local OS provider.
    pub(super) fn open_process_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(feature = "process-provider")]
        self.open_provider_process_picker("processmemory", window, cx);
        #[cfg(all(not(feature = "process-provider"), feature = "memflow-provider"))]
        self.open_memflow_attach_dialog(window, cx);
        #[cfg(all(not(feature = "process-provider"), not(feature = "memflow-provider")))]
        self.open_process_picker(window, cx);
    }

    #[cfg(feature = "memflow-provider")]
    pub(super) fn open_memflow_attach_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.active_editor_or_notify("Open a document first.", window, cx)
        else {
            return;
        };
        let dialog = crate::ui::dialogs::MemflowAttachDialog::view(window, cx);
        self.goto_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            move |this, _dialog, ev: &crate::ui::dialogs::MemflowAttachEvent, window, cx| match ev {
                crate::ui::dialogs::MemflowAttachEvent::Cancel => window.close_dialog(cx),
                crate::ui::dialogs::MemflowAttachEvent::Attach(cfg) => {
                    let target = match cfg.to_target() {
                        Ok(target) => target,
                        Err(err) => {
                            this.notify(err, window, cx);
                            return;
                        }
                    };
                    let provider = match this
                        .plugin_manager
                        .create_provider("memflowprocessmemory", &target)
                    {
                        Ok(provider) => provider,
                        Err(err) => {
                            this.notify(format!("memflow attach failed: {err}"), window, cx);
                            return;
                        }
                    };
                    let display = provider.name();
                    editor.update(cx, |ed, cx| {
                        ed.controller_mut()
                            .attach_provider_with_identifier_and_target(
                                provider,
                                "memflowprocessmemory",
                                true,
                                target,
                            );
                        ed.apply_document(cx);
                    });
                    let source = crate::ui::state::DataSource::new(
                        crate::ui::state::SourceKind::Process,
                        display.clone(),
                    );
                    this.set_active_source(source, window, cx);
                    this.settings
                        .borrow_mut()
                        .set(settings_keys::LAST_ATTACHED_PROCESS, display.as_str());
                    window.close_dialog(cx);
                    this.notify(format!("Attached {display}"), window, cx);
                }
            },
        ));
        self.present_modal(&dialog, 720., 80., None, window, cx);
    }

    /// File ▸ Data Source ▸ Process Memory — open the legacy process picker (the
    /// C++ `ProcessPicker` reached from `selectSource("process")`). Live process
    /// attach is handled first-party by the sibling memflow path when
    /// `memflow-provider` is enabled; this older picker is a non-attaching fallback
    /// that surfaces registry rows and records the selection as the document's
    /// logical source.
    #[allow(dead_code)]
    pub(super) fn open_process_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_provider_process_picker("processmemory", window, cx);
    }

    pub(super) fn open_provider_process_picker(
        &mut self,
        provider_identifier: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::pickers::processpicker::{
            ProcessPickEvent, ProcessPicker, ProcessPickerModel,
        };
        let Some(editor) = self.active_editor_or_notify("Open a document first.", window, cx)
        else {
            return;
        };
        let Some(spec) = self.plugin_manager.provider_spec(provider_identifier) else {
            self.notify(
                format!("No provider registered as {provider_identifier}."),
                window,
                cx,
            );
            return;
        };
        let Some(processes) = spec.enumerate_processes() else {
            self.notify(
                format!("{provider_identifier} does not provide a process list."),
                window,
                cx,
            );
            return;
        };
        let model = ProcessPickerModel::from_processes(processes, provider_identifier);
        // Remember which process the user last attached to (the C++
        // `lastAttachedProcess` QSettings key; processpicker.cpp:386-403). The
        // picker READS this to pre-select the matching row by name in
        // `selectPreferredProcess`; we thread the loaded value in so that read→
        // pre-select half is wired (the WRITE side is the `set(...)` on a
        // successful attach, below). `None`/empty → first-attachable preference.
        let last_attached = self
            .settings
            .borrow()
            .get(settings_keys::LAST_ATTACHED_PROCESS)
            .filter(|s| !s.is_empty());
        let picker = cx.new(|cx| ProcessPicker::new(model, last_attached, window, cx));
        self.goto_sub = Some(cx.subscribe_in(
            &picker,
            window,
            move |this, _p, ev: &ProcessPickEvent, window, cx| match ev {
                ProcessPickEvent::Attach {
                    identifier,
                    name,
                    pid,
                    ..
                } => {
                    window.close_dialog(cx);
                    let target = match identifier.as_str() {
                        "remoteprocessmemory" => format!("rpm:{pid}:{name}"),
                        "kernelmemory" => format!("km:{pid}:{name}"),
                        _ => format!("{pid}:{name}"),
                    };
                    #[cfg(feature = "remote-process-provider")]
                    if identifier == "remoteprocessmemory" {
                        window.close_dialog(cx);
                        this.open_remote_connect_choice(
                            editor.clone(),
                            identifier.to_string(),
                            target,
                            *pid,
                            name.to_string(),
                            window,
                            cx,
                        );
                        return;
                    }
                    window.close_dialog(cx);
                    this.attach_provider_target(&editor, identifier.as_str(), target, window, cx);
                }
                ProcessPickEvent::Cancel => window.close_dialog(cx),
            },
        ));
        self.present_modal(&picker, 720., 80., None, window, cx);
    }

    fn attach_provider_target(
        &mut self,
        editor: &Entity<crate::ui::editor::RcxEditor>,
        identifier: &str,
        target: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let provider = match self.plugin_manager.create_provider(identifier, &target) {
            Ok(provider) => provider,
            Err(err) => {
                self.notify(format!("Attach failed: {err}"), window, cx);
                return;
            }
        };
        let display = provider.name();
        editor.update(cx, |ed, cx| {
            ed.controller_mut()
                .attach_provider_with_identifier_and_target(provider, identifier, true, target);
            ed.apply_document(cx);
        });
        self.settings
            .borrow_mut()
            .set(settings_keys::LAST_ATTACHED_PROCESS, display.as_str());
        let source = crate::ui::state::DataSource::new(
            crate::ui::state::SourceKind::Process,
            display.clone(),
        );
        self.set_active_source(source, window, cx);
        self.notify(format!("Attached {display}"), window, cx);
    }

    #[cfg(feature = "remote-process-provider")]
    fn open_remote_connect_choice(
        &mut self,
        editor: Entity<crate::ui::editor::RcxEditor>,
        identifier: String,
        target: String,
        pid: u32,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dialog = cx.new(|cx| RemoteConnectDialog::new(name.clone(), pid, cx));
        let focus = dialog.read(cx).focus_handle(cx);
        self.goto_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            move |this, _d, choice: &RemoteConnectChoice, window, cx| match choice {
                RemoteConnectChoice::Cancel => window.close_dialog(cx),
                RemoteConnectChoice::AlreadyInjected => {
                    window.close_dialog(cx);
                    this.attach_provider_target(&editor, &identifier, target.clone(), window, cx);
                }
                RemoteConnectChoice::InjectPayload => {
                    window.close_dialog(cx);
                    if let Err(err) = crate::provider::RemoteProcessProvider::inject_payload(pid) {
                        this.notify(format!("Injection failed: {err}"), window, cx);
                        return;
                    }
                    this.attach_provider_target(&editor, &identifier, target.clone(), window, cx);
                }
            },
        ));
        self.present_modal(&dialog, 460., 100., Some(&focus), window, cx);
    }

    /// File ▸ Data Source ▸ WinDbg Memory — collect the C++ plugin target string
    /// and attach through the provider registry (`tcp:...`, `npipe:...`,
    /// `pid:<id>`, or `dump:<path>`).
    #[cfg(all(windows, feature = "windbg-provider"))]
    pub(super) fn open_windbg_attach_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.active_editor_or_notify("Open a document first.", window, cx)
        else {
            return;
        };
        let this = cx.entity().downgrade();
        self.open_text_prompt(
            "WinDbg Memory",
            "Target",
            "tcp:Port=5055,Server=localhost",
            window,
            cx,
            move |target, window, app| {
                let target = target.trim().to_string();
                if target.is_empty() {
                    return;
                }
                let _ = this.update(app, |me, cx| {
                    let provider = match me.plugin_manager.create_provider("windbgmemory", &target)
                    {
                        Ok(provider) => provider,
                        Err(err) => {
                            me.notify(format!("WinDbg attach failed: {err}"), window, cx);
                            return;
                        }
                    };
                    let display = provider.name();
                    editor.update(cx, |ed, cx| {
                        ed.controller_mut()
                            .attach_provider_with_identifier_and_target(
                                provider,
                                "windbgmemory",
                                true,
                                target,
                            );
                        ed.apply_document(cx);
                    });
                    let source = crate::ui::state::DataSource::new(
                        crate::ui::state::SourceKind::Process,
                        display.clone(),
                    );
                    me.set_active_source(source, window, cx);
                    me.settings
                        .borrow_mut()
                        .set(settings_keys::LAST_ATTACHED_PROCESS, display.as_str());
                    me.notify(format!("Attached {display}"), window, cx);
                });
            },
        );
    }

    /// File ▸ Data Source ▸ unavailable provider — surface a modal warning when a
    /// platform-specific provider was compiled out or the ReClass.NET compat layer
    /// is unavailable on this platform.
    pub(super) fn report_unavailable_source(
        &mut self,
        cmd: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = match cmd {
            "source.remote" => "Remote Process Memory",
            "source.kernel" => "Kernel Memory",
            "source.windbg" => "WinDbg Memory",
            "source.rcnet" => "ReClass.NET Compat",
            _ => "This data source",
        };
        let spec = crate::ui::dialogs::messagebox::warn(
            "Source Unavailable",
            &format!(
                "{label} is not available in this build or on this operating system. \
                 Open a project with a saved source, or attach another data source."
            ),
        );
        crate::ui::dialogs::messagebox::open_message(spec, window, cx);
    }

    /// File ▸ Data Source ▸ Clear All — detach the active document's source (the
    /// C++ `clearSources`).
    pub(super) fn clear_active_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.document_area.read(cx).active_editor().cloned() {
            editor.update(cx, |ed, cx| {
                ed.controller_mut().clear_sources();
                ed.apply_document(cx);
            });
        }
        self.set_active_source(crate::ui::state::DataSource::none(), window, cx);
    }

    // ── Edit: bookmarks (the C++ promptAddBookmark / Quick Bookmark Here) ──

    /// Edit ▸ Add Bookmark… (Ctrl+B) — prompt for a name (defaulting the formula
    /// to the active doc's base) and add the bookmark (the C++
    /// `promptAddBookmark`; main.cpp:8090). The themed prompt collects the name;
    /// the formula defaults to the current base.
    pub(super) fn prompt_add_bookmark(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor_or_notify("Open a document first.", window, cx)
        else {
            return;
        };
        let default_formula = {
            let ed = editor.read(cx);
            let tree = ed.controller().tree();
            if tree.base_address_formula.is_empty() {
                format!("0x{:X}", tree.base_address)
            } else {
                tree.base_address_formula.clone()
            }
        };
        // Free-text name entry (the C++ Edit ▸ Add Bookmark `QInputDialog::getText`
        // collects the bookmark NAME; the formula defaults to the current base).
        // Seed the field with the next free auto-name so a blind Enter still works,
        // but let the user type any name — replacing the old auto-name-only confirm.
        let default_name = self.next_bookmark_name(&editor, cx);
        let editor2 = editor.clone();
        let formula = default_formula.clone();
        let this = cx.entity().downgrade();
        self.open_text_prompt(
            "Add Bookmark",
            &format!("Name (address {default_formula})"),
            &default_name,
            window,
            cx,
            move |name, window, app| {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return;
                }
                let _ = this.update(app, |me, cx| {
                    editor2.update(cx, |ed, _cx| {
                        ed.controller_mut().add_bookmark(&name, &formula);
                    });
                    me.after_bookmark_added(&name, &formula, window, cx);
                });
            },
        );
    }

    /// Edit ▸ Quick Bookmark Here (Ctrl+Alt+B) — capture the current address as an
    /// auto-named `bookmark_NN` (no dialog; the C++ lambda at main.cpp:1197).
    pub(super) fn quick_bookmark_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor_or_notify("Open a document first.", window, cx)
        else {
            return;
        };
        let formula = {
            let ed = editor.read(cx);
            let tree = ed.controller().tree();
            if tree.base_address_formula.is_empty() {
                format!("0x{:X}", tree.base_address)
            } else {
                tree.base_address_formula.clone()
            }
        };
        let name = self.next_bookmark_name(&editor, cx);
        editor.update(cx, |ed, _cx| {
            ed.controller_mut().add_bookmark(&name, &formula);
        });
        self.after_bookmark_added(&name, &formula, window, cx);
    }

    /// Find a free `bookmark_NN` slot name in the active document (the C++
    /// taken-set loop; main.cpp:1204).
    pub(super) fn next_bookmark_name(
        &self,
        editor: &Entity<crate::ui::editor::RcxEditor>,
        cx: &Context<Self>,
    ) -> String {
        let taken: std::collections::HashSet<String> = editor
            .read(cx)
            .controller()
            .tree()
            .bookmarks
            .iter()
            .map(|b| b.name.clone())
            .collect();
        let mut n = 1;
        loop {
            let name = format!("bookmark_{n:02}");
            if !taken.contains(&name) || n >= 1000 {
                return name;
            }
            n += 1;
        }
    }

    /// Shared tail after a bookmark is added: refresh the bookmarks dock, open the
    /// right dock, sync dirty state, and confirm (the C++ `refreshBookmarksDock` +
    /// `setAppStatus`).
    pub(super) fn after_bookmark_added(
        &mut self,
        name: &str,
        formula: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_right_dock_open(true, window, cx);
        self.sync_view_menu_checked(cx);
        // Populate the bookmarks dock with the controller's current list (the C++
        // `refreshBookmarksDock`); previously the panel stayed empty after Add.
        self.refresh_docks_for_active(cx);
        self.sync_dirty_state(cx);
        self.notify(format!("Bookmarked: {name} → {formula}"), window, cx);
        cx.notify();
    }

    /// Handle a bookmarks-dock row action (the C++ bookmark-row click → navigate,
    /// right-click → Navigate / Remove): navigate the active editor to the
    /// bookmark's address formula, or remove the bookmark at its index + refresh
    /// the dock.
    pub(super) fn on_bookmark_action(
        &mut self,
        ev: crate::ui::panels::bookmarkspanel::BookmarkAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::panels::bookmarkspanel::BookmarkAction;
        match ev {
            BookmarkAction::Navigate { formula } => {
                // Reuse the go-to-address resolve/navigate path so `<mod>+0x..` /
                // `[ptr]` forms resolve against the live provider.
                self.commit_goto(&formula, 0, window, cx);
            }
            BookmarkAction::Remove { index } => {
                if let Some(editor) = self.document_area.read(cx).active_editor().cloned() {
                    editor.update(cx, |ed, cx| {
                        ed.controller_mut().remove_bookmark(index);
                        ed.apply_document(cx);
                    });
                }
                self.refresh_docks_for_active(cx);
                self.sync_dirty_state(cx);
                cx.notify();
            }
            // Header "+" → open the same Add-Bookmark prompt the Edit menu uses.
            BookmarkAction::Add => self.prompt_add_bookmark(window, cx),
        }
    }

    /// Navigate the active editor to an absolute address (scanner result-row jump
    /// target): rebase the active tree to `addr` + recompose.
    pub(super) fn navigate_active_editor_to_address(
        &mut self,
        addr: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.active_editor_or_notify("Open a document first.", window, cx)
        else {
            return;
        };
        editor.update(cx, |ed, cx| {
            let ctrl = ed.controller_mut();
            {
                let tree = &mut ctrl.document_mut().tree;
                tree.base_address = addr;
                tree.base_address_formula.clear();
            }
            // Reset value-history / heat on a jump (the C++ `resetChangeTracking`
            // on navigate): the old base's per-node change heat + history no longer
            // describes the new region, so clear it before recomposing — otherwise
            // every field flashes "changed" against the previous address's values.
            ctrl.reset_change_tracking();
            ed.apply_document(cx);
        });
        self.rebuild_workspace(cx);
        self.refresh_docks_for_active(cx);
        self.notify(format!("Jumped to 0x{addr:X}"), window, cx);
        cx.notify();
    }

    /// Modules-panel row activation: open/reuse a top-level class for the module,
    /// set the view base to the module base, and force absolute gutter addresses
    /// so the selected image base is visible immediately.
    pub(super) fn jump_active_editor_to_module_base(
        &mut self,
        base: u64,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.active_editor_or_notify("Open a document first.", window, cx)
        else {
            return;
        };
        self.set_view_option_value(ViewOpt::RelativeOffsets, false, cx);
        let (root_id, type_name, created) = editor.update(cx, |ed, cx| {
            let result = ed.controller_mut().open_module_root_class(name, base);
            ed.apply_document(cx);
            result
        });
        if let Some(t) = self.state.active_tab_mut() {
            t.view_root = Some(root_id);
        }
        self.rebuild_workspace(cx);
        self.refresh_docks_for_active(cx);
        if created {
            self.sync_dirty_state(cx);
        }
        if name.is_empty() {
            self.notify(
                format!("Opened {type_name} at module base 0x{base:X}"),
                window,
                cx,
            );
        } else {
            self.notify(
                format!("Opened {type_name} at {name}+0x0 (base 0x{base:X})"),
                window,
                cx,
            );
        }
        cx.notify();
    }

    /// Resolve a scanner result-cell inline edit against the active document (the
    /// C++ `onCellEdited`):
    /// - **Address cell** (`EvalAddress`): re-evaluate the (re-typed) address
    ///   expression with the live provider callbacks, re-read the value at the new
    ///   address, and push it back into the row (`apply_address_edit`) so the row
    ///   tracks the new location.
    /// - **Value cell** (`WriteValue`): serialize the value-input text with the
    ///   scanner's value type and write it back to the row's address through the
    ///   active controller's writable provider, then refresh the row's recorded
    ///   bytes (`apply_value_write`).
    ///
    /// The panel raises the intent because it holds only a read-only `Arc` provider
    /// clone; the mutable controller lives here on the window.
    pub(super) fn on_scanner_edit(
        &mut self,
        scanner: Entity<crate::ui::panels::scannerpanel::ScannerPanel>,
        ev: crate::ui::panels::scannerpanel::ScannerEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::panels::scannerpanel::ScannerEdit;
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            self.notify("Attach a data source first.", window, cx);
            return;
        };
        match ev {
            ScannerEdit::EvalAddress { row } => {
                // The re-typed address expression lives in the scanner's value
                // input (the inline-edit source the panel exposes); evaluate it,
                // re-read the value at the resolved address, and rebind the row.
                let formula = scanner.read(cx).value_input_text(cx);
                if formula.trim().is_empty() {
                    return;
                }
                let value_type = scanner.read(cx).last_value_type();
                let ptr_size = editor.read(cx).controller().tree().pointer_size.max(1);
                let provider = editor.read(cx).controller().document().provider.clone();
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
                                (p.read_u64(addr), true)
                            } else {
                                (p.read_u32(addr) as u64, true)
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
                let r = crate::addr::AddressParser::evaluate(&formula, ptr_size, Some(&cbs));
                if !r.ok {
                    self.notify(format!("Couldn't evaluate \"{formula}\"."), window, cx);
                    return;
                }
                let size = crate::scanner::value_size_for_type(value_type).max(0);
                let new_value = if size > 0 {
                    provider.read_bytes(r.value, size)
                } else {
                    Vec::new()
                };
                scanner.update(cx, |sp, cx| {
                    sp.apply_address_edit(row, r.value, new_value, cx);
                });
            }
            ScannerEdit::WriteValue { row, address } => {
                let text = scanner.read(cx).value_input_text(cx);
                if text.trim().is_empty() {
                    self.notify(
                        "Type a value in the scanner's Value field, then double-click \
                         a result's Value cell to write it.",
                        window,
                        cx,
                    );
                    return;
                }
                let value_type = scanner.read(cx).last_value_type();
                // Serialize the typed value to bytes via the scanner value type
                // (the C++ writes the typed value, not the displayed bytes).
                let bytes = match crate::scanner::serialize_value(value_type, &text) {
                    Ok((b, _mask)) => b,
                    Err(e) => {
                        self.notify(format!("Invalid value: {e}"), window, cx);
                        return;
                    }
                };
                if bytes.is_empty() {
                    return;
                }
                let wrote = editor.update(cx, |ed, _cx| {
                    ed.controller_mut().write_memory(address, &bytes)
                });
                if wrote {
                    let new_value = bytes.clone();
                    scanner.update(cx, |sp, cx| {
                        sp.apply_value_write(row, new_value, cx);
                    });
                    // The write changed live memory — re-read so the editor reflects
                    // the new value.
                    editor.update(cx, |ed, cx| ed.apply_document(cx));
                } else {
                    self.notify(
                        "Write failed — the data source isn't writable (open a File source).",
                        window,
                        cx,
                    );
                }
            }
        }
    }

    /// Scanner "Add as Nodes" — append one node per result address into the active
    /// editor's view-root container (the C++ drag-result-into-editor / add-as-nodes
    /// path; scannerpanel.cpp:655). Each append is a tail `Hex64` field via the
    /// controller (the same path the editor's own add-member uses). Reports the
    /// count added; rebuilds the workspace + dirty dot like any other mutation.
    pub(super) fn on_scanner_add_nodes(
        &mut self,
        addresses: &[u64],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            self.notify("Open a document first to add scanner nodes.", window, cx);
            return;
        };
        if addresses.is_empty() {
            return;
        }
        let added = editor.update(cx, |ed, cx| {
            let view_root = ed.controller().view_root_id();
            // Append into the view-root container in one controller command.
            // The older loop called `append_single_field` once per scanner row,
            // which recomposed after every insert before this single UI repaint.
            let added = ed
                .controller_mut()
                .append_single_fields(view_root, addresses.len())
                .len();
            ed.apply_document(cx);
            added
        });
        self.rebuild_workspace(cx);
        self.sync_dirty_state(cx);
        self.notify(format!("Added {added} node(s) from scanner"), window, cx);
        cx.notify();
    }

    /// Scanner "Change All Values" — write `bytes` to every result address through
    /// the active editor's mutable controller, re-read each, and hand the results
    /// back to [`ScannerPanel::apply_change_all`] so it produces the "Wrote to X/Y"
    /// tail (the C++ batch write + re-read; scannerpanel.cpp:981). A failed write
    /// (read-only provider) yields `None` for that address so the count reflects
    /// only the writes that landed.
    pub(super) fn on_scanner_batch_edit(
        &mut self,
        scanner: Entity<crate::ui::panels::scannerpanel::ScannerPanel>,
        ev: crate::ui::panels::scannerpanel::ScannerBatchEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            self.notify("Attach a data source first.", window, cx);
            return;
        };
        if ev.bytes.is_empty() || ev.addresses.is_empty() {
            return;
        }
        let read_size = ev.bytes.len() as i32;
        let provider = editor.read(cx).controller().document().provider.clone();
        let mut results: Vec<(u64, Option<Vec<u8>>)> = Vec::with_capacity(ev.addresses.len());
        for &addr in &ev.addresses {
            let wrote = editor.update(cx, |ed, _cx| {
                ed.controller_mut().write_memory(addr, &ev.bytes)
            });
            if wrote {
                // Re-read the just-written bytes so the scanner row reflects the
                // new value (the C++ `prov->readBytes(r.address, readSize)`).
                let nb = provider.read_bytes(addr, read_size);
                results.push((addr, Some(nb)));
            } else {
                results.push((addr, None));
            }
        }
        // A live write changed memory under the editor — recompose so it reflects.
        editor.update(cx, |ed, cx| ed.apply_document(cx));
        scanner.update(cx, |sp, cx| sp.apply_change_all(results, cx));
        cx.notify();
    }

    // ── View: font family (the C++ exclusive Consolas / JetBrains Mono picker) ──

    /// View ▸ Font ▸ {Consolas / JetBrains Mono} — set + persist the editor font
    /// family (the C++ `setEditorFont` + settings("font"); main.cpp:5071). The
    /// editor surface owns no live family setter in this port, so the window owns
    /// the selection + persisted setting + the Font submenu ✓.
    pub(super) fn set_editor_font(
        &mut self,
        family: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor_font = family.to_string();
        // Persist the selection to the disk store so it survives a relaunch (the
        // C++ `settings.setValue("font", family)`; main.cpp:1311). Loaded back in
        // the ctor.
        self.settings.borrow_mut().set(settings_keys::FONT, family);
        self.sync_font_menu_checked(cx);
        // Push the chosen family into every open editor surface (the C++
        // `setEditorFont` applies the font to all editors; main.cpp:5071).
        // Previously this only persisted the key + synced the menu ✓, so the
        // Font submenu was cosmetic — no editor ever changed font. Split panes
        // view the SAME active editor entity, so iterating the tab editors
        // covers every visible pane.
        let family_ss: SharedString = family.to_string().into();
        let editors: Vec<Entity<crate::ui::editor::RcxEditor>> = self
            .document_area
            .read(cx)
            .tabs()
            .iter()
            .map(|t| t.editor.clone())
            .collect();
        for editor in editors {
            editor.update(cx, |ed, cx| {
                ed.set_font_family(Some(family_ss.clone()), cx);
            });
        }
        self.notify(format!("Editor font: {family}"), window, cx);
    }

    /// Push the active font family into the Font submenu ✓ (exclusive — only the
    /// active family is checked).
    pub(super) fn sync_font_menu_checked(&mut self, cx: &mut Context<Self>) {
        let consolas = self.editor_font == "Consolas";
        self.menubar.update(cx, |mb, cx| {
            mb.set_command_checked("view.font.consolas", consolas, cx);
            mb.set_command_checked("view.font.jetbrains", !consolas, cx);
        });
    }
}
