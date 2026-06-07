//! Small shared helpers — active-doc id resolution, `DataSource` derivation from a
//! document/controller, path→title + ReClass-XML path sniffing, theme switch by
//! index, and the generic text-prompt opener. Extracted from window.rs as an
//! `impl super::MainWindow` block; `use super::*` inherits the parent's imports.

use super::*;

impl super::MainWindow {
    /// The active document id, preferring the `DocumentArea` (the source of truth
    /// for tab ordering) and falling back to [`AppState`].
    pub(super) fn active_doc_id(&self, cx: &Context<Self>) -> Option<DocId> {
        self.document_area
            .read(cx)
            .active_entry()
            .map(|e| e.id)
            .or_else(|| self.state.active_id())
    }

    /// Map a loaded document's provider/data path to the UI [`DataSource`]
    /// summary (the tab source-icon; app-shell §8 `refreshDocTabSourceIcon`).
    pub(super) fn source_for_doc(
        doc: &crate::controller::RcxDocument,
    ) -> crate::ui::state::DataSource {
        use crate::ui::state::{DataSource, SourceKind};
        match &doc.data_path {
            Some(p) => DataSource::new(SourceKind::File, p.to_string_lossy().into_owned()),
            None => DataSource::none(),
        }
    }

    /// The UI [`DataSource`] for a controller AFTER it has ingested its saved
    /// sources — preferring the active saved-source entry (its display name +
    /// kind), falling back to the document's attached `data_path`. The C++ tab
    /// source icon reads the active source post-attach (`refreshDocTabSourceIcon`
    /// runs after `ingestPendingSavedSources`); reading the doc BEFORE the
    /// controller ingested left the icon showing "no source" even when values
    /// loaded (the bug). For a File source we keep the on-disk path so the icon +
    /// tooltip match the attached binary.
    pub(super) fn source_for_controller(
        ctrl: &crate::controller::RcxController,
    ) -> crate::ui::state::DataSource {
        use crate::ui::state::{DataSource, SourceKind};
        let idx = ctrl.active_source_index();
        if idx >= 0 {
            if let Some(entry) = ctrl.saved_sources().get(idx as usize) {
                if entry.kind == "File" {
                    let path = if entry.file_path.is_empty() {
                        entry.display_name.clone()
                    } else {
                        entry.file_path.clone()
                    };
                    return DataSource::new(SourceKind::File, path);
                } else if entry.kind == "Process" || entry.kind == "processmemory" {
                    return DataSource::new(SourceKind::Process, entry.display_name.clone());
                } else if entry.kind == "Buffer" {
                    return DataSource::new(SourceKind::Buffer, entry.display_name.clone());
                } else if entry.kind == "Snapshot" {
                    return DataSource::new(SourceKind::Snapshot, entry.display_name.clone());
                }
            }
        }
        Self::source_for_doc(ctrl.document())
    }

    /// The tab title for an opened project file — its stem (the C++ titles a tab
    /// by the file/struct name; here the file stem is the stable, faithful
    /// choice and matches `updateWindowTitle`). Falls back to `"Untitled"`.
    pub(super) fn title_for_path(path: &std::path::Path) -> String {
        path.file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "Untitled".to_string())
    }

    /// Probe a file for the ReClass-XML byte signature (the C++ `project_open`
    /// sniff; main.cpp:6123-6131). Opens the file, reads the first 64 bytes
    /// (matching `probe.read(64)`), and delegates to [`sniff_is_reclass_xml`]. On
    /// any open/read failure returns `false` — the C++ leaves `isXml=false` when
    /// the probe `QFile` fails to open, falling through to the native JSON load.
    ///
    /// An associated fn (no `&self`) so the byte-sniff is testable without a window.
    pub(super) fn path_is_reclass_xml(path: &std::path::Path) -> bool {
        use std::io::Read as _;
        let Ok(mut f) = std::fs::File::open(path) else {
            return false;
        };
        let mut head = [0u8; 64];
        match f.read(&mut head) {
            Ok(n) => sniff_is_reclass_xml(&head[..n]),
            Err(_) => false,
        }
    }

    /// Switch the active theme by index and re-apply it (themes.md §4.4
    /// `setCurrent` → §4.16 `themeChanged` → re-style). Updates the state's
    /// theme handle. No-op if the index is out of range.
    pub fn switch_theme(&mut self, index: usize, window: &mut Window, cx: &mut App) {
        let applied = {
            let mut tm = self.theme_manager.borrow_mut();
            tm.set_current(index);
            tm.current().clone()
        };
        self.state.set_theme_name(&applied.name);
        crate::ui::theme_apply::apply_theme(&applied, window, cx);
    }

    /// Open a modal free-text input prompt (the C++ `QInputDialog::getText`):
    /// title + field label + a seeded value. On accept (Enter / OK) the trimmed
    /// text is delivered to `on_accept`; Cancel / Esc dismisses with no callback.
    /// Used by the bookmark-name and type-rename flows (replacing the previous
    /// auto-name confirm boxes).
    pub(super) fn open_text_prompt<F>(
        &mut self,
        title: &str,
        label: &str,
        default: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_accept: F,
    ) where
        F: Fn(String, &mut Window, &mut App) + 'static,
    {
        let title = title.to_string();
        let label = label.to_string();
        let default = default.to_string();
        let prompt = cx.new(|cx| {
            TextPromptDialog::new(title.clone(), label.clone(), default.clone(), window, cx)
        });
        let on_accept = Rc::new(on_accept);
        self.goto_sub = Some(cx.subscribe_in(
            &prompt,
            window,
            move |_this, _p, ev: &TextPromptEvent, window, cx| match ev {
                TextPromptEvent::Accept(text) => {
                    let text = text.clone();
                    window.close_dialog(cx);
                    // DEFER the accept callback so it runs OUTSIDE this subscription's
                    // MainWindow update. The callbacks re-acquire the window via
                    // `this.update(...)`; calling that here (already inside a MainWindow
                    // update via `subscribe_in`'s `&mut self`) panics with "cannot update
                    // MainWindow while it is already being updated" — e.g. the workspace
                    // Rename, whose action now actually dispatches.
                    let on_accept = on_accept.clone();
                    window.defer(cx, move |window, cx| (on_accept)(text, window, cx));
                }
                TextPromptEvent::Cancel => window.close_dialog(cx),
            },
        ));
        let focus = prompt.read(cx).focus_handle(cx);
        self.present_modal(&prompt, 420., 140., Some(&focus), window, cx);
    }
}

/// The struct name of the active **view root** for the window title (the C++
/// `rootName(tree, viewRootId())`; main.cpp:5180). Climbs from the view-root
/// node to its top-level parent and returns that node's `struct_type_name`
/// (falling back to its `name`). Mirrors the status-bar's `root_name_of` climb.
/// Returns an empty string when the index is out of range. Pure; unit-tested.
pub(crate) fn root_name_for_title(tree: &crate::core::NodeTree, view_root_id: u64) -> String {
    let mut cur = tree.index_of_id(view_root_id);
    // A `0`/unknown view root means "the whole document" — start at the first
    // top-level node so a fresh document still names its root struct.
    if cur < 0 {
        cur = tree
            .nodes
            .iter()
            .position(|n| n.parent_id == 0)
            .map_or(-1, |i| i as i32);
    }
    while cur >= 0 {
        let Some(n) = tree.nodes.get(cur as usize) else {
            break;
        };
        if n.parent_id == 0 {
            return if n.struct_type_name.is_empty() {
                n.name.clone()
            } else {
                n.struct_type_name.clone()
            };
        }
        cur = tree.index_of_id(n.parent_id);
    }
    String::new()
}

/// Detect a ReClass-XML file from its leading bytes (the C++ `project_open`
/// signature sniff; main.cpp:6129): after trimming leading ASCII whitespace, the
/// head starts with `<?xml` or `<ReClass`. The C++ used
/// `head.trimmed().startsWith(...)` — `QByteArray::trimmed()` strips both ends,
/// but only the leading run matters for a prefix test, so trimming the start is
/// equivalent. Chosen by content, not by file *name*: a `.rcx` carrying XML is
/// imported as XML and an `.xml` carrying JSON falls through to the native JSON
/// load (the headline of this gap). Pure; unit-tested.
pub(crate) fn sniff_is_reclass_xml(head: &[u8]) -> bool {
    let trimmed = head.trim_ascii_start();
    trimmed.starts_with(b"<?xml") || trimmed.starts_with(b"<ReClass")
}
