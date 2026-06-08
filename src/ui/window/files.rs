//! File I/O cluster — open/import (native pickers), save/write, code export, the
//! built-in examples, document close, and module-symbol download. Extracted from
//! window.rs as an `impl super::MainWindow` block; `use super::*` inherits the
//! parent's imports.

use super::*;

/// File ▸ Import ▸ … target — picks the importer + the file-picker prompt text.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ImportKind {
    Source,
    Xml,
    Pdb,
}

impl ImportKind {
    pub(crate) fn prompt(self) -> &'static str {
        match self {
            ImportKind::Source => "Import C/C++ source",
            ImportKind::Xml => "Import ReClass XML",
            ImportKind::Pdb => "Import PDB",
        }
    }
}

/// File ▸ Export ▸ … target — picks the [`crate::generator`] renderer + the
/// suggested file extension.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ExportKind {
    Cpp,
    Rust,
    Defines,
    CSharp,
    Python,
    Xml,
}

impl ExportKind {
    /// The suggested save-file extension (incl. the leading dot).
    pub(crate) fn extension(self) -> &'static str {
        match self {
            ExportKind::Cpp => ".h",
            ExportKind::Rust => ".rs",
            ExportKind::Defines => ".h",
            ExportKind::CSharp => ".cs",
            ExportKind::Python => ".py",
            ExportKind::Xml => ".xml",
        }
    }

    /// The [`CodeFormat`](crate::generator::CodeFormat) this code export maps to,
    /// or `None` for the ReClass-XML export (which goes through the importer's
    /// file exporter, not the generator).
    pub(crate) fn code_format(self) -> Option<crate::generator::CodeFormat> {
        use crate::generator::CodeFormat;
        Some(match self {
            ExportKind::Cpp => CodeFormat::CppHeader,
            ExportKind::Rust => CodeFormat::RustStruct,
            ExportKind::Defines => CodeFormat::DefineOffsets,
            ExportKind::CSharp => CodeFormat::CSharpStruct,
            ExportKind::Python => CodeFormat::PythonCtypes,
            ExportKind::Xml => return None,
        })
    }

    /// The save-file dialog filter for this export (the C++
    /// `codeFormatFileFilter(fmt)`; main.cpp:5758). XML uses a fixed ReClass-XML
    /// filter; everything else routes through
    /// [`code_format_file_filter`](crate::generator::code_format_file_filter).
    pub(crate) fn file_filter(self) -> &'static str {
        match self.code_format() {
            Some(fmt) => crate::generator::code_format_file_filter(fmt),
            None => "ReClass XML (*.xml);;All Files (*)",
        }
    }

    /// Render the active document's tree to this format. Mirrors the C++
    /// `exportToFile` (main.cpp:5752-5774), which always calls `renderCodeAll`
    /// (the **full SDK** — every root struct, ignoring the current view root) with
    /// the document's `typeAliases` and the persisted `generatorAsserts` flag.
    /// Returns `None` when nothing was produced.
    pub(crate) fn render(
        self,
        tree: &crate::core::NodeTree,
        aliases: Option<&crate::generator::TypeAliases>,
        emit_asserts: bool,
    ) -> Option<String> {
        let Some(fmt) = self.code_format() else {
            return Self::render_xml(tree);
        };
        let text = crate::generator::render_code_all(fmt, tree, aliases, emit_asserts);
        if text.trim().is_empty() {
            None
        } else {
            Some(text)
        }
    }

    /// ReClass XML export goes through the importer module's exporter, which is
    /// file-based — render to a temp file, read it back as the export text.
    /// Gated on the `imports` feature.
    #[cfg(feature = "imports")]
    fn render_xml(tree: &crate::core::NodeTree) -> Option<String> {
        let mut path = std::env::temp_dir();
        path.push("reclass-export.xml");
        crate::imports::export_reclass_xml(tree, &path).ok()?;
        let text = std::fs::read_to_string(&path).ok();
        let _ = std::fs::remove_file(&path);
        text.filter(|t| !t.trim().is_empty())
    }

    #[cfg(not(feature = "imports"))]
    fn render_xml(_tree: &crate::core::NodeTree) -> Option<String> {
        None
    }
}

impl super::MainWindow {
    // (section comments + methods moved verbatim from the original impl)
    // ── File: open / import (native file pickers) ───────────────────────────

    /// File ▸ Open… — the native path picker (`cx.prompt_for_paths`); on a chosen
    /// `.rcx`/`.xml`, route it through [`open_project`](Self::open_project) (the
    /// same plumbing the start page + CLI use). Cancelled / errored → no-op.
    pub(super) fn prompt_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open Reclass project".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = rx.await.ok().and_then(|r| r.ok()).flatten() else {
                return;
            };
            let Some(path) = path.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |me, window, cx| {
                me.open_project(&path, None, window, cx);
            });
        })
        .detach();
    }

    /// File ▸ Import ▸ {From Source / ReClass XML / PDB} — pick a file, then build
    /// a document from it via the importers and load it into the active tab.
    /// XML routes through [`open_project`](Self::open_project) (its `.xml` branch);
    /// Source/PDB go through [`import_into_active`](Self::import_into_active).
    pub(super) fn prompt_import(
        &mut self,
        kind: ImportKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(kind.prompt().into()),
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
            let _ = this.update_in(cx, |me, window, cx| match kind {
                ImportKind::Xml => {
                    me.open_project(&path, None, window, cx);
                }
                ImportKind::Source | ImportKind::Pdb => {
                    me.import_into_active(kind, &path, window, cx);
                }
            });
        })
        .detach();
    }

    /// Build a document from a Source / PDB file and load it into the active tab
    /// (the C++ `import_from_source` / `import_pdb`). Gated on the `imports`
    /// feature; a parse failure notifies and leaves the current document intact.
    #[cfg(feature = "imports")]
    pub(super) fn import_into_active(
        &mut self,
        kind: ImportKind,
        path: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tree = match kind {
            ImportKind::Source => match std::fs::read_to_string(path) {
                Ok(src) => crate::imports::import_from_source(&src, 8),
                Err(e) => {
                    self.notify(format!("Could not read source: {e}"), window, cx);
                    return;
                }
            },
            ImportKind::Pdb => crate::imports::import_pdb(path, ""),
            ImportKind::Xml => return,
        };
        match tree {
            Ok(tree) => {
                let mut doc = crate::controller::RcxDocument::new();
                doc.tree = tree;
                self.load_doc_into_active(doc, path, window, cx);
            }
            Err(e) => self.notify(format!("Import failed: {e}"), window, cx),
        }
    }

    #[cfg(not(feature = "imports"))]
    pub(super) fn import_into_active(
        &mut self,
        _kind: ImportKind,
        _path: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.notify("Importing requires the `imports` feature.", window, cx);
    }

    // ── File: save / export ─────────────────────────────────────────────────

    /// File ▸ Save / Save As… — write the active document's tree as native `.rcx`
    /// JSON. "Save" reuses the known file path when present; "Save As…" (or a
    /// never-saved document) prompts for a new path via the native picker.
    pub(super) fn save_active(
        &mut self,
        force_prompt: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            return;
        };
        let known = editor.read(cx).controller().document().file_path.clone();
        if let (false, Some(path)) = (force_prompt, known) {
            self.write_document(&editor, &path, window, cx);
            return;
        }
        let suggested = editor
            .read(cx)
            .controller()
            .document()
            .file_path
            .as_ref()
            .and_then(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "Untitled.rcx".to_string());
        let dir = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
        let rx = cx.prompt_for_new_path(&dir, Some(&suggested));
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = rx.await.ok().and_then(|r| r.ok()).flatten() else {
                return;
            };
            let _ = this.update_in(cx, |me, window, cx| {
                me.write_document(&editor, &path, window, cx);
            });
        })
        .detach();
    }

    /// Persist the editor's document to `path` and reflect the new title/source.
    pub(super) fn write_document(
        &mut self,
        editor: &Entity<crate::ui::editor::RcxEditor>,
        path: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ok = editor.update(cx, |ed, _cx| ed.controller_mut().document_mut().save(path));
        if ok {
            let title = Self::title_for_path(path);
            if let Some(id) = self.active_doc_id(cx) {
                self.document_area.update(cx, |area, cx| {
                    area.set_title(id, title.clone(), cx);
                });
                self.state.set_title(id, title);
            }
            // Record the just-saved path in the recent-files list (the C++ `saveFile`
            // / `saveFileAs` call `addRecentFile(path)` after a successful write;
            // main.cpp). Without this, Save As to a new path never surfaced it in
            // Recent Files or the start page until the next Open.
            self.record_recent_file(path, cx);
            self.notify(format!("Saved {}", path.display()), window, cx);
        } else {
            self.notify(format!("Failed to save {}", path.display()), window, cx);
        }
    }

    /// File ▸ Export ▸ … — render the active document's tree to the chosen code
    /// format ([`crate::generator`] / the XML exporter) and prompt a save path.
    /// Mirrors the C++ `exportToFile` (main.cpp:5752-5774): it exports the **full
    /// SDK** (`renderCodeAll` — every root struct, ignoring the current view root)
    /// with the document's `typeAliases` and the persisted `generatorAsserts`
    /// flag, and offers the format's [`code_format_file_filter`] in the save
    /// dialog (GPUI's `prompt_for_new_path` has no filter slot, so the filter
    /// drives only the suggested extension).
    pub(super) fn export_code(
        &mut self,
        kind: ExportKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            self.notify("No document to export.", window, cx);
            return;
        };
        let emit_asserts = self.generator_asserts;
        let rendered = {
            let ed = editor.read(cx);
            let ctrl = ed.controller();
            // The document's per-kind name overrides (the C++ `tab->doc->typeAliases`,
            // passed as `nullptr` when empty; main.cpp:5761-5762).
            let aliases = &ctrl.document().type_aliases;
            let aliases = if aliases.is_empty() {
                None
            } else {
                Some(aliases)
            };
            kind.render(ctrl.tree(), aliases, emit_asserts)
        };
        let Some(text) = rendered else {
            self.notify("Nothing to export from this document.", window, cx);
            return;
        };
        // The C++ passes `codeFormatFileFilter(fmt)` to the save dialog; GPUI has
        // no filter parameter, so we surface it through the suggested extension.
        let _filter = kind.file_filter();
        let suggested = format!("export{}", kind.extension());
        let dir = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
        let rx = cx.prompt_for_new_path(&dir, Some(&suggested));
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = rx.await.ok().and_then(|r| r.ok()).flatten() else {
                return;
            };
            let _ = this.update_in(cx, |me, window, cx| match std::fs::write(&path, &text) {
                Ok(()) => me.notify(format!("Exported {}", path.display()), window, cx),
                Err(e) => me.notify(format!("Export failed: {e}"), window, cx),
            });
        })
        .detach();
    }

    // ── File: examples / close ──────────────────────────────────────────────

    /// File ▸ Examples ▸ <NAME> — materialize the bundled example to a temp file
    /// and open it through the normal window plumbing (the start-page Examples
    /// bucket). Opening a bundled example must WORK (the stage contract).
    pub(super) fn open_example(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        match crate::ui::examples::write_example_to_temp(name) {
            Some(path) => {
                self.open_project(&path, None, window, cx);
            }
            None => self.notify(format!("Unknown example: {name}"), window, cx),
        }
    }

    /// File ▸ Close Project (Ctrl+W) — close the active document tab. If the
    /// active document is modified, show the 3-way unsaved-changes guard first
    /// (the C++ `closeFile` → unsaved prompt): **Save** persists the active doc
    /// then closes (aborting on a save failure), **Discard** closes without
    /// saving, **Cancel** aborts. The document area never leaves a blank window
    /// (it re-seeds a fresh tab when the last closes).
    pub(super) fn close_active_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.document_area.read(cx).active_editor().cloned();
        let dirty: Vec<(Entity<crate::ui::editor::RcxEditor>, String)> = match active {
            Some(editor) if editor.read(cx).controller().document().modified => {
                let ed = editor.read(cx);
                let ctrl = ed.controller();
                let doc = ctrl.document();
                let name = match &doc.file_path {
                    Some(p) => p
                        .file_name()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| root_name_for_title(ctrl.tree(), ctrl.view_root_id())),
                    None => root_name_for_title(ctrl.tree(), ctrl.view_root_id()),
                };
                vec![(editor.clone(), name)]
            }
            _ => Vec::new(),
        };
        if dirty.is_empty() {
            self.do_close_active(window, cx);
            return;
        }
        self.open_unsaved_guard(
            dirty,
            |me, window, cx| me.do_close_active(window, cx), // Saved → close.
            |me, window, cx| me.do_close_active(window, cx), // Discard → close.
            window,
            cx,
        );
    }

    /// Actually close the active tab (after any unsaved-changes guard).
    pub(super) fn do_close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.document_area.read(cx).active_index();
        self.document_area.update(cx, |area, cx| {
            area.close_index(ix, window, cx);
        });
        // The area emits `Closed`/`NewDocumentRequested` which our doc-area event
        // handler mirrors into AppState + rebuilds the workspace.
        cx.notify();
    }

    /// Modules dock ▸ Download All (the C++ `download_all`): load/download PDB
    /// symbols for every module of the active source. Enumerate the active
    /// provider's modules; for each, prefer an already-cached or module-adjacent
    /// PDB (the C++ `findCached`/`findLocal`, which need no network). A real
    /// network fetch needs each module's PE debug GUID/age — only obtainable from
    /// a live process target (see `crate::provider::memflow`), so report what was
    /// resolvable and how many modules were seen. With no modules (a File source
    /// enumerates none) this guides the user to attach a live source.
    #[cfg(feature = "symbols")]
    pub(super) fn download_all_module_symbols(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            self.notify("Attach a data source first.", window, cx);
            return;
        };
        let modules = editor
            .read(cx)
            .controller()
            .document()
            .provider
            .enumerate_modules();
        if modules.is_empty() {
            self.notify(
                "No modules to download symbols for — attach a live process source \
                 (file sources expose no loaded modules).",
                window,
                cx,
            );
            return;
        }
        let downloader = crate::rtti::downloader::SymbolDownloader::new();
        let mut resolved = 0usize;
        for m in &modules {
            // Without PE debug info we can't form a download request; but if a PDB
            // is already cached or sits next to the module we can count it as
            // resolvable (the C++ short-circuits on findCached/findLocal first).
            let pdb_name = if m.name.to_ascii_lowercase().ends_with(".pdb") {
                m.name.clone()
            } else {
                // Best-effort: <module-stem>.pdb beside the module image.
                let stem = std::path::Path::new(&m.name)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&m.name);
                format!("{stem}.pdb")
            };
            if crate::rtti::downloader::SymbolDownloader::find_local(&m.full_path, &pdb_name)
                .is_some()
            {
                resolved += 1;
                continue;
            }
            let _ = &downloader; // (network fetch needs live PE debug GUID/age)
        }
        self.notify(
            format!(
                "Download All: {resolved}/{} module symbols resolved from cache/local. \
                 Network PDB fetch needs a live process target.",
                modules.len()
            ),
            window,
            cx,
        );
    }

    #[cfg(not(feature = "symbols"))]
    pub(super) fn download_all_module_symbols(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.notify(
            "Download All symbols requires the symbols feature.",
            window,
            cx,
        );
    }
}
