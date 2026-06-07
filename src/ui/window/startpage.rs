//! Start page + project-open cluster — showing/dismissing the welcome overlay,
//! building its recent-entry list, routing its events, and the project open path
//! (open_project + ReClass-XML import). Extracted from window.rs as an
//! `impl super::MainWindow` block; `use super::*` inherits the parent's imports.

use super::*;

impl super::MainWindow {
    /// Show the start-page welcome overlay over the workspace (`showStartPage`).
    /// No-op if already shown. Mirrors the C++ reflex of preloading a New Class
    /// behind the splash (main.cpp:9383-9384) so dismissing lands on an editable
    /// class, not a blank editor.
    pub fn show_start_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.start_page.is_some() {
            return;
        }
        self.preload_new_class_if_empty(window, cx);
        let entries = self.recent_entries();
        let page = StartPage::view(entries, window, cx);
        cx.subscribe_in(
            &page,
            window,
            |this, _page, ev: &StartPageEvent, window, cx| {
                this.on_start_page_event(ev.clone(), window, cx);
            },
        )
        .detach();
        self.start_page = Some(page);
        cx.notify();
    }

    /// Preload a New Class behind the splash when the active document is still the
    /// blank initial doc — the C++ `showStartPage` reflex `if (m_tabs.isEmpty())
    /// newClass()` (main.cpp:9383-9384). The Rust shell always holds the initial
    /// tab, so the equivalent guard is "the active document has no top-level struct
    /// yet". Seeds the same New Class the File ▸ New Class command does, retitles
    /// the tab, and resyncs the view options / workspace / docks.
    fn preload_new_class_if_empty(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            return;
        };
        let is_empty = !editor
            .read(cx)
            .controller()
            .tree()
            .nodes
            .iter()
            .any(|n| n.parent_id == 0 && n.kind == crate::core::NodeKind::Struct);
        if !is_empty {
            return;
        }
        editor.update(cx, |ed, cx| {
            ed.set_document(seed_root_doc(RootKind::Class), cx)
        });
        if let Some(id) = self.document_area.read(cx).active_id() {
            self.document_area.update(cx, |area, cx| {
                area.set_title(id, RootKind::Class.title(), cx)
            });
        }
        self.apply_view_opts_to_editor(&editor, cx);
        self.rebuild_workspace(cx);
        self.refresh_docks_for_active(cx);
    }

    /// Dismiss the start page (`dismissStartPage`).
    pub fn dismiss_start_page(&mut self, cx: &mut Context<Self>) {
        if self.start_page.take().is_some() {
            cx.notify();
        }
    }

    /// The recent-files entries for the start page, built from the session
    /// recent-files list (the C++ `recentFiles` QSettings, surfaced by the start
    /// page). Most-recent-first; each entry's age is the whole-day delta between
    /// now and the file's last-modified time (the C++ `buildGroups` compared
    /// `QFileInfo::lastModified()` against `now`; startpage.h:236), so the start
    /// page buckets recent files into Today / Yesterday / This Week / This Month /
    /// Older. On a metadata/time error the age falls back to 0 (today's bucket).
    pub(super) fn recent_entries(&self) -> Vec<RecentEntry> {
        use std::time::{SystemTime, UNIX_EPOCH};
        // Sample the clock once so every entry buckets against the same "now".
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // Skip entries whose file no longer exists (the C++ start-page filters
        // the same way the Recent Files menu does; main.cpp:8789).
        let mut entries: Vec<RecentEntry> = self
            .existing_recent_files()
            .into_iter()
            .map(|(_, p)| {
                // Whole-day age from the file's mtime (UNIX seconds). Any
                // metadata/time failure → age 0 (today), matching the C++ "no
                // timestamp ⇒ treat as recent" fallthrough.
                let age_days = std::fs::metadata(p)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| {
                        crate::ui::chrome::startpage::age_days_from_secs(now_secs, d.as_secs())
                    })
                    .unwrap_or(0);
                RecentEntry {
                    path: p.to_string_lossy().into_owned(),
                    file_name: p
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("(file)")
                        .to_string(),
                    dir_path: p
                        .parent()
                        .map(|d| d.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    age_days,
                    is_example: false,
                }
            })
            .collect();
        // Append the bundled examples (the C++ `loadEntries` always lists the
        // examples regardless of the recent list; startpage.h). These give the
        // start page a one-click "Continue" demo to open even on a fresh install
        // with no recent files — the audited "start-page Continue demo absent"
        // gap. Each example's `path` is its `file.example.<name>` key, routed
        // through `open_example` (not `open_project`) by `on_start_page_event`.
        entries.extend(crate::ui::chrome::startpage::example_entries());
        entries
    }

    pub(super) fn on_start_page_event(
        &mut self,
        ev: StartPageEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::chrome::startpage::StartCard;
        match ev {
            StartPageEvent::Dismissed => {
                self.dismiss_start_page(cx);
            }
            // ROUTE EACH CARD DISTINCTLY (the bug: every card landed on a new
            // class). Each card now does its OWN thing — the same operation its
            // File-menu twin runs.
            //
            // CRITICAL: do NOT dismiss the splash here for the async file-picker
            // cards. `prompt_open`/`prompt_import` spawn a native dialog that
            // resolves *later*; dismissing eagerly would tear the welcome page
            // down to the blank "Untitled" doc the instant the card is clicked,
            // so a cancelled (or, headless, un-openable) dialog would strand the
            // user on an empty document instead of returning to the splash. Only
            // the SUCCESS paths dismiss: `open_project` (window.rs ~1597) and
            // `load_doc_into_active` (~953) both call `dismiss_start_page` once a
            // project actually loads. `NewClass` is synchronous and unconditional,
            // so it still dismisses eagerly (inside `new_document`).
            StartPageEvent::Card(card) => match card {
                StartCard::NewClass => self.new_document(RootKind::Class, window, cx),
                StartCard::OpenProject => self.prompt_open(window, cx),
                StartCard::ImportSource => self.prompt_import(ImportKind::Source, window, cx),
                StartCard::ImportXml => self.prompt_import(ImportKind::Xml, window, cx),
                StartCard::ImportPdb => self.prompt_import(ImportKind::Pdb, window, cx),
            },
            StartPageEvent::FileSelected(path) => {
                // A bundled-example row carries a `file.example.<name>` key (not a
                // real on-disk path); route it through `open_example`
                // (materialize-then-open) — the C++ Examples bucket opens the
                // example, not a non-existent path. A real recent file → the C++
                // `project_open(path)`. Either way the splash dismisses.
                self.dismiss_start_page(cx);
                if let Some(name) = path.strip_prefix("file.example.") {
                    self.open_example(name, window, cx);
                } else {
                    self.open_project(std::path::Path::new(&path), None, window, cx);
                }
            }
        }
    }

    // ── Document lifecycle (the C++ `project_open` / `loadData`; app-shell §8) ──

    /// Open a `.rcx` project into the **active** document tab (the C++
    /// `project_open(path)`; main.cpp:6370). Loads the tree via the controller's
    /// [`RcxDocument::load`](crate::controller::RcxDocument::load) (the forgiving
    /// 8-step JSON load), optionally attaches a binary data source
    /// (`loadData(dataPath)`), refreshes the editor, then syncs the window state:
    /// the tab title (from the file stem, the C++ `rootName`), the data-source
    /// icon, and the workspace tree. The start page is dismissed so the user
    /// lands on the loaded document.
    ///
    /// `.xml` paths are routed through the ReClass-XML importer
    /// ([`crate::imports::import_reclass_xml`]) when the `imports` feature is on;
    /// every other extension is treated as native `.rcx` JSON. Returns `true` on
    /// a successful load (the tab now shows the project), `false` otherwise (the
    /// existing document is left untouched — the C++ "refuse rather than show a
    /// placeholder", controller.cpp:201).
    pub fn open_project(
        &mut self,
        path: &std::path::Path,
        data_path: Option<&std::path::Path>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use crate::controller::RcxDocument;

        // Build the document from disk. ReClass-XML imports go through the XML
        // importer (when compiled in); `.rcx`/`.json`/anything else is the native
        // forgiving JSON load. On any failure we leave the current doc untouched.
        //
        // The importer is chosen by the file's leading BYTES (the C++ 64-byte
        // signature probe; main.cpp:6123-6131), not by extension: an `.rcx`
        // holding XML imports as XML, and an `.xml` holding JSON loads natively.
        let mut doc = RcxDocument::new();
        let is_xml = Self::path_is_reclass_xml(path);

        let loaded = if is_xml {
            self.load_reclass_xml(path, &mut doc)
        } else {
            doc.load(path)
        };
        if !loaded {
            tracing::warn!(path = %path.display(), "open_project: load failed — keeping current document");
            return false;
        }
        doc.file_path = Some(path.to_path_buf());

        // Optionally attach the binary data source (`--data` / saved source).
        // When no explicit --data path is given the controller AUTO-ATTACHES the
        // active saved File source (resolved relative to the .rcx dir in `load`)
        // inside `set_document → RcxController::new → ingestPendingSavedSources`,
        // so the headline "all-0x0" case (a .rcx with a savedSources File entry)
        // now populates with real values. We therefore read the resulting source
        // BACK from the controller AFTER `set_document`, not from `doc` before it
        // (the bug: `doc.data_path` is still None until the controller ingests).
        if let Some(dp) = data_path {
            doc.load_data_file(dp);
        }

        // REPLACE-ALL into a fresh tab (the C++ `project_open` does NOT rebind the
        // active tab — it closes every existing doc dock then creates a brand-new
        // tab bound to the loaded document: `{ ClosingGuard guard(m_closingAll);
        // closeAllDocDocks(); dock = createTab(doc); }`; main.cpp:6147-6150 /
        // 6190-6194). `replace_all_with_fresh` drops the prior tabs (signalling each
        // `Closed` so the per-tab state unwinds) and returns the fresh id + editor;
        // the loaded document is then driven into THAT editor.
        let title = Self::title_for_path(path);
        let (fresh_id, editor) = self.document_area.update(cx, |area, cx| {
            area.replace_all_with_fresh(title.clone(), window, cx)
        });
        // Mirror the replace-all into AppState SYNCHRONOUSLY so its tab list +
        // monotonic id counter stay in lockstep with the area. The area's `Closed`
        // events are delivered later (deferred), and — unlike a `+`-tab open —
        // `replace_all_with_fresh` emits NO `NewDocumentRequested`, so the fresh
        // doc must be registered in state here, not via the event. Both id
        // allocators were equal before this call, so closing every state doc then
        // opening one fresh yields the SAME id the area just allocated; the later
        // `Closed(old_id)` events then no-op on the already-removed docs.
        let old_ids: Vec<DocId> = self.state.tabs().iter().map(|t| t.id).collect();
        for id in old_ids {
            self.state.close_document(id);
        }
        let state_fresh_id = self.state.open_document(title.clone());
        debug_assert_eq!(
            state_fresh_id, fresh_id,
            "AppState + DocumentArea id allocators must stay in lockstep"
        );
        editor.update(cx, |ed, cx| ed.set_document(doc, cx));
        // Realign the fresh editor to the window's (persisted) view options — a new
        // doc otherwise inherits controller/view defaults that disagree.
        self.apply_view_opts_to_editor(&editor, cx);
        // Derive the source icon from the controller's NOW-attached provider
        // (post-ingest), so a .rcx's saved File source shows the File icon.
        let source = Self::source_for_controller(editor.read(cx).controller());

        // Sync the tab title (file stem ≈ the C++ `rootName`) + source icon into
        // both the document area and the window state, keyed on the FRESH id.
        self.document_area.update(cx, |area, cx| {
            area.set_title(fresh_id, title.clone(), cx);
            area.set_source(fresh_id, source.clone(), cx);
        });
        self.state.set_title(fresh_id, title);
        self.state.set_source(fresh_id, source);

        // Refresh the workspace tree from the freshly loaded document + dismiss
        // the start page so the user lands on the document.
        self.rebuild_workspace(cx);
        // Force the workspace dock open (the C++ `placeSidebarDock(m_workspaceDock,
        // LeftDockWidgetArea); m_workspaceDock->show()` runs unconditionally after a
        // successful open; main.cpp:6152-6155 / 6196-6199). Done here — on the
        // open_project path only, not on every `set_document` — so an open always
        // reveals the project tree while plain in-editor reloads leave the layout
        // alone.
        self.apply_layout_preset(LayoutPreset::Workspace, window, cx);
        self.dismiss_start_page(cx);
        // Feed the freshly-attached provider into the scanner/modules docks and
        // the document's bookmarks into the bookmarks dock.
        self.refresh_docks_for_active(cx);
        // Record this project as a recent file (the C++ `addRecentFile`) so the
        // File ▸ Recent Files submenu + start page surface it on next open, and
        // propagate the (clean) dirty state into the tab.
        self.record_recent_file(path, cx);
        self.sync_dirty_state(cx);
        cx.notify();
        true
    }

    /// Load a ReClass-XML file into `doc.tree` (the C++ `import_reclass_xml`
    /// path). Gated on the `imports` feature; without it, XML opens fail
    /// gracefully (returns `false`).
    #[cfg(feature = "imports")]
    pub(super) fn load_reclass_xml(
        &self,
        path: &std::path::Path,
        doc: &mut crate::controller::RcxDocument,
    ) -> bool {
        match crate::imports::import_reclass_xml(path, 8) {
            Ok(tree) => {
                doc.tree = tree;
                true
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "import_reclass_xml failed");
                false
            }
        }
    }

    #[cfg(not(feature = "imports"))]
    pub(super) fn load_reclass_xml(
        &self,
        _path: &std::path::Path,
        _doc: &mut crate::controller::RcxDocument,
    ) -> bool {
        tracing::warn!("ReClass-XML import requires the `imports` feature");
        false
    }
}
