// Headless tests for the pure dispatch helpers added with the action-wiring.
// These import specific items (NOT `super::*`) so the module's `gpui::*` glob
// is not pulled into the test-hygiene expansion (see the menubar.rs note).
use super::{
    dirty_doc_name, root_name_for_title, seed_root_doc, settings_keys, sniff_is_reclass_xml,
    unique_dirty_names, unsaved_changes_text, window_title_string, DiskSettings, ExportKind,
    ImportKind, RootKind, ViewOpt, ViewOptions, ABOUT_GITHUB_URL,
};
use crate::theme::SettingsStore;
use std::cell::RefCell;
use std::rc::Rc;

fn temp_settings_path() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    p.push(format!("reclass-settings-test-{n}.json"));
    p
}

fn builtin_ids() -> Vec<&'static str> {
    let mut ids = vec!["file"];
    #[cfg(feature = "process-provider")]
    ids.push("processmemory");
    #[cfg(feature = "remote-process-provider")]
    ids.push("remoteprocessmemory");
    #[cfg(all(windows, feature = "kernel-provider"))]
    ids.push("kernelmemory");
    #[cfg(all(windows, feature = "windbg-provider"))]
    ids.push("windbgmemory");
    #[cfg(feature = "memflow-provider")]
    ids.push("memflowprocessmemory");
    ids.extend(["buffer", "snapshot", "null"]);
    ids
}

fn builtin_enabled_without_null() -> Vec<&'static str> {
    builtin_ids()
        .into_iter()
        .filter(|id| *id != "null")
        .collect()
}

#[test]
fn disk_settings_round_trips_scalars_lists_and_bools_across_reopen() {
    let path = temp_settings_path();
    {
        let mut s = DiskSettings::open_at(path.clone());
        s.set("font", "Consolas");
        s.set_bool("minimap", true);
        s.set_list(
            "recentFiles",
            &["/a/one.rcx".to_string(), "/b/two.rcx".to_string()],
        );
    }
    // Reopen from disk — every value must survive (the QSettings semantics).
    let s2 = DiskSettings::open_at(path.clone());
    assert_eq!(s2.get("font").as_deref(), Some("Consolas"));
    assert!(s2.get_bool("minimap", false));
    assert_eq!(
        s2.get_list("recentFiles"),
        vec!["/a/one.rcx".to_string(), "/b/two.rcx".to_string()]
    );
    // A missing key falls back to the supplied default.
    assert!(s2.get_bool("definitely-missing", true));
    assert!(s2.get_list("definitely-missing").is_empty());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn disk_settings_empty_list_round_trips_as_empty() {
    let path = temp_settings_path();
    let mut s = DiskSettings::open_at(path.clone());
    // Empty + whitespace-only entries are dropped so they round-trip empty.
    s.set_list("recentFiles", &["".to_string()]);
    assert!(s.get_list("recentFiles").is_empty());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn view_options_load_reads_persisted_keys_and_defaults_the_rest() {
    let path = temp_settings_path();
    let mut s = DiskSettings::open_at(path.clone());
    // Persist a NON-default subset; the rest must fall back to C++ defaults.
    s.set_bool(settings_keys::TYPE_HINTS, true); // default false
    s.set_bool(settings_keys::COMPACT_COLUMNS, false); // default true
    let o = ViewOptions::load(&s);
    assert!(o.type_hints, "persisted typeHints=true must load");
    assert!(
        !o.compact_columns,
        "persisted compactColumns=false must load"
    );
    // Unset keys keep the defaults.
    assert!(o.tree_lines);
    assert!(o.relative_offsets);
    assert!(o.hover_effects);
    assert!(!o.show_comments);
    assert!(!o.minimap);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn view_option_persist_key_matches_cpp_qsettings_key() {
    // The keys MUST match the C++ QSettings spellings (main.cpp:1336-1411)
    // so a project edited in either build reads the other's saved toggles.
    assert_eq!(ViewOptions::key(ViewOpt::CompactColumns), "compactColumns");
    assert_eq!(ViewOptions::key(ViewOpt::TreeLines), "treeLines");
    assert_eq!(
        ViewOptions::key(ViewOpt::RelativeOffsets),
        "relativeOffsets"
    );
    assert_eq!(ViewOptions::key(ViewOpt::TypeHints), "typeHints");
    assert_eq!(ViewOptions::key(ViewOpt::ShowComments), "showComments");
    assert_eq!(ViewOptions::key(ViewOpt::HoverEffects), "hoverEffects");
    assert_eq!(ViewOptions::key(ViewOpt::Minimap), "minimap");
}

#[test]
fn source_for_controller_reads_active_saved_file_source() {
    // The #3 fix: after a controller ingests a saved File source, the tab
    // icon must read the ACTIVE source (its file path), not the doc's
    // pre-ingest data_path. Build a controller with a savedSources File
    // entry pointing at a real on-disk file and assert the derived source.
    use crate::controller::{RcxController, RcxDocument};
    use serde_json::json;

    // A real sidecar file so the auto-attach does not bail.
    let data = temp_settings_path().with_extension("bin");
    std::fs::write(&data, b"hello").unwrap();

    let mut doc = RcxDocument::new();
    doc.pending_saved_sources.push(json!({
        "kind": "File",
        "displayName": "sample.bin",
        "filePath": data.to_string_lossy(),
    }));
    let ctrl = RcxController::new(doc);
    let src = super::MainWindow::source_for_controller(&ctrl);
    assert_eq!(src.kind, crate::ui::state::SourceKind::File);
    assert_eq!(src.target, data.to_string_lossy());
    let _ = std::fs::remove_file(&data);
}

// ── Dynamic window title (the C++ updateWindowTitle) ──

#[test]
fn window_title_formats_name_dirty_and_empty() {
    // `"<name> - Reclass"` when clean; a trailing `" *"` before " - Reclass"
    // when modified; plain "Reclass" when the name is empty (no document).
    assert_eq!(window_title_string("Player", false), "Player - Reclass");
    assert_eq!(window_title_string("Player", true), "Player * - Reclass");
    assert_eq!(window_title_string("", false), "Reclass");
    assert_eq!(window_title_string("", true), "Reclass");
}

// ── Unsaved-changes guard (the C++ closeEvent; item 1) ──

#[test]
fn unsaved_changes_text_picks_sentence_by_count() {
    // The C++ uses two complete sentences keyed on the distinct-dirty-doc
    // count (main.cpp:9003-9006): singular for one, "%1 projects …" otherwise.
    assert_eq!(unsaved_changes_text(1), "One project has unsaved changes:");
    assert_eq!(unsaved_changes_text(2), "2 projects have unsaved changes:");
    assert_eq!(unsaved_changes_text(7), "7 projects have unsaved changes:");
}

#[test]
fn unique_dirty_names_dedups_preserving_first_seen_order() {
    // The C++ `dirtyNames` skips repeats (a doc shared across tabs is listed
    // once; main.cpp:8994-8995) while keeping discovery order.
    let names = vec![
        "Player".to_string(),
        "World".to_string(),
        "Player".to_string(), // duplicate (shared doc) → dropped
        "Enemy".to_string(),
    ];
    assert_eq!(
        unique_dirty_names(names),
        vec![
            "Player".to_string(),
            "World".to_string(),
            "Enemy".to_string()
        ]
    );
    // Empty in → empty out (nothing dirty).
    assert!(unique_dirty_names(Vec::<String>::new()).is_empty());
}

// ── A1: window-close guard decision logic (the C++ closeEvent; main.cpp:8989) ──

#[test]
fn clean_document_yields_no_dirty_name_so_close_is_accepted() {
    // Port of the C++ `closeEvent` early-out (main.cpp:8989, 8998): an
    // unmodified document contributes NO dirty name. `collect_dirty_docs`
    // filters each tab through `dirty_doc_name`; a set of only-clean docs
    // therefore collects empty ⇒ `guarded_window_close` returns true (the Qt
    // `event->accept()` — close immediately, no prompt). `seed_root_doc` builds
    // a fresh doc which starts `modified == false`.
    let doc = seed_root_doc(RootKind::Class);
    assert!(!doc.modified, "a freshly seeded doc starts clean");
    let root_id = doc.tree.nodes.iter().find(|n| n.parent_id == 0).unwrap().id;
    assert_eq!(
        dirty_doc_name(doc.modified, doc.file_path.as_deref(), &doc.tree, root_id),
        None,
        "a clean doc must not enter the dirty set (allow-close path)"
    );
}

#[test]
fn dirty_unsaved_document_names_by_view_root_struct() {
    // The C++ name rule for a dirty, never-saved doc: the view-root struct name
    // (main.cpp:8991, the `filePath.isEmpty()` branch). A dirty doc DOES enter
    // the set (the prompt path).
    let doc = seed_root_doc(RootKind::Class);
    let root_id = doc.tree.nodes.iter().find(|n| n.parent_id == 0).unwrap().id;
    assert_eq!(
        dirty_doc_name(true, None, &doc.tree, root_id),
        Some(root_name_for_title(&doc.tree, root_id)),
        "an unsaved dirty doc names by its view-root struct"
    );
}

#[test]
fn dirty_saved_document_names_by_file_basename() {
    // The C++ name rule for a dirty, saved doc: the file BASENAME (main.cpp:8993,
    // `QFileInfo(filePath).fileName()`), not the struct name and not the full path.
    let doc = seed_root_doc(RootKind::Class);
    let root_id = doc.tree.nodes.iter().find(|n| n.parent_id == 0).unwrap().id;
    let path = std::path::PathBuf::from("/home/u/projects/Player.rcx");
    assert_eq!(
        dirty_doc_name(true, Some(path.as_path()), &doc.tree, root_id),
        Some("Player.rcx".to_string()),
        "a saved dirty doc names by its file basename"
    );
}

// ── Help ▸ About GitHub URL (the C++ about() "Open GitHub"; item 8) ──

#[test]
fn about_github_url_points_at_ichooseyou_repo() {
    // The C++ About dialog opens https://github.com/IChooseYou/Reclass
    // (main.cpp:4434), NOT the old reclassnet/reclass URL.
    assert_eq!(ABOUT_GITHUB_URL, "https://github.com/IChooseYou/Reclass");
    assert!(!ABOUT_GITHUB_URL.contains("reclassnet"));
}

#[test]
fn root_name_for_title_uses_view_root_struct_name() {
    // The window title names the active VIEW ROOT's top-level struct: build a
    // class document and assert its struct_type_name is returned for the root,
    // and that climbing from a child reaches the same root name.
    let doc = seed_root_doc(RootKind::Class);
    let tree = &doc.tree;
    // The root is the first top-level node; its view-root id names the title.
    let root_id = tree.nodes.iter().find(|n| n.parent_id == 0).unwrap().id;
    let name = root_name_for_title(tree, root_id);
    assert_eq!(name, RootKind::Class.type_name());
    // A `0`/unknown view root still resolves to the first top-level struct.
    assert_eq!(root_name_for_title(tree, 0), RootKind::Class.type_name());
    // Climbing from a child field reaches the same root name.
    if let Some(child) = tree.nodes.iter().find(|n| n.parent_id == root_id) {
        assert_eq!(
            root_name_for_title(tree, child.id),
            RootKind::Class.type_name()
        );
    }
}

// ── Plugins manager (the read-only port of showPluginsDialog) ──

#[test]
fn builtin_plugins_lists_the_provider_backends() {
    let plugins = crate::ui::dialogs::plugin_manager::builtin_plugins();
    // Every shipped provider backend is an in-tree built-in (the Phase-6
    // detected-kind label, design §4/§6).
    assert!(!plugins.is_empty());
    assert!(plugins.iter().all(|p| p.kind == "builtin"));
    // The built-ins auto-load (LoadType::Auto) → all shown enabled (design
    // §7.A [fix] enabled-state disclosure).
    assert!(plugins.iter().all(|p| p.enabled));
    let names: Vec<&str> = plugins.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"File Provider"));
    assert!(names.contains(&"Null Provider"));
    // Each row carries the fields the C++ dialog shows.
    assert!(plugins
        .iter()
        .all(|p| { !p.version.is_empty() && !p.author.is_empty() && !p.description.is_empty() }));
    // Every row carries its routing identifier (so the dialog can toggle it).
    assert!(plugins.iter().all(|p| !p.identifier.is_empty()));
}

// ── F1: session-owned PluginManager + DiskSettings-backed persistence ──
//
// These exercise the EXACT production wiring `MainWindow::new` builds — a
// `PluginManager::with_persistence_and_builtins` over a `DiskPluginPersistence`
// backed by the real `DiskSettings` (settings.json) — but at a temp path, so no
// GPUI window is needed to assert the enable/disable + persistence behaviour.

use crate::plugin::{DiskPluginPersistence, PluginManager};

/// Build the session manager exactly as the window does, over a DiskSettings at
/// `path` (coerced to the shared `SettingsStore` trait object).
fn session_manager_at(path: &std::path::Path) -> PluginManager {
    let settings: Rc<RefCell<dyn SettingsStore>> =
        Rc::new(RefCell::new(DiskSettings::open_at(path.to_path_buf())));
    PluginManager::with_persistence_and_builtins(Box::new(DiskPluginPersistence::new(settings)))
}

#[test]
fn session_manager_parity_with_empty_settings() {
    // PARITY: a fresh config dir → the registry is exactly the
    // Auto-enabled built-ins, identical order, all enabled — byte-identical to
    // the old throwaway `with_builtins()` the live sites used.
    let path = temp_settings_path();
    let mgr = session_manager_at(&path);
    let ids: Vec<&str> = mgr
        .registry()
        .providers()
        .iter()
        .map(|p| p.identifier.as_str())
        .collect();
    assert_eq!(ids, builtin_ids());
    assert!(mgr
        .registry()
        .providers()
        .iter()
        .all(|p| p.is_builtin && p.enabled));
    assert_eq!(
        mgr.registry().enabled_providers().count(),
        builtin_ids().len()
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn disk_backed_enable_disable_reflected_in_view_registry_and_persisted() {
    let path = temp_settings_path();
    {
        // Disable a built-in via the manager with persist=true (the dialog's
        // Toggle path).
        let mut mgr = session_manager_at(&path);
        assert!(mgr.set_enabled("null", false, true));

        // Reflected in plugins_view (what the dialog renders)…
        let rows = mgr.plugins_view();
        let null = rows.iter().find(|r| r.identifier == "null").unwrap();
        assert!(!null.enabled);
        // …and in the registry's enabled_providers (what the pickers read).
        let enabled: Vec<&str> = mgr
            .registry()
            .enabled_providers()
            .map(|p| p.identifier.as_str())
            .collect();
        assert_eq!(enabled, builtin_enabled_without_null());
    }
    // The flag landed in settings.json under the namespaced key.
    let s = DiskSettings::open_at(path.clone());
    assert_eq!(s.get("plugin.enabled.null").as_deref(), Some("false"));

    // RESTART: a brand-new session manager over the SAME file restores the
    // disabled built-in (the persistence round-trip end-to-end).
    let mgr2 = session_manager_at(&path);
    assert!(!mgr2.registry().find("null").unwrap().enabled);
    assert_eq!(
        mgr2.registry().enabled_providers().count(),
        builtin_ids().len() - 1
    );

    // Re-enabling persists too, so a third session sees it back on.
    {
        let mut mgr3 = session_manager_at(&path);
        assert!(mgr3.set_enabled("null", true, true));
    }
    let mgr4 = session_manager_at(&path);
    assert!(mgr4.registry().find("null").unwrap().enabled);
    assert_eq!(
        mgr4.registry().enabled_providers().count(),
        builtin_ids().len()
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn plugin_infos_from_rows_preserves_builtin_display_and_identifier() {
    // The dialog's view-model mapper: built-ins get the C++ "<name> Provider"
    // text, the routing identifier is carried, and the enabled flag tracks the
    // manager (here a disabled built-in shows disabled).
    let path = temp_settings_path();
    let mut mgr = session_manager_at(&path);
    assert!(mgr.set_enabled("null", false, true));
    let infos = crate::ui::dialogs::plugin_manager::plugin_infos_from_rows(mgr.plugins_view());

    let file = infos.iter().find(|i| i.identifier == "file").unwrap();
    assert_eq!(file.name, "File Provider");
    assert!(file.enabled);
    assert_eq!(file.kind, "builtin");

    let null = infos.iter().find(|i| i.identifier == "null").unwrap();
    assert_eq!(null.name, "Null Provider");
    assert!(
        !null.enabled,
        "disabled built-in shows disabled in the dialog"
    );
    // Built-ins carry is_builtin = true (so the dialog shows no Unload control).
    assert!(infos.iter().all(|i| i.is_builtin));
    let _ = std::fs::remove_file(&path);
}

// ── F2: live-host safe-unload + the dialog's row refresh contract ──
//
// The window-backed `LivePluginHost` is gpui-bound (it needs a `Window`/`App`),
// so following the repo convention (no gpui-bound tests — see the tabs.rs note)
// these assert the NON-gpui logic the `Unload` arm runs: the manager's
// safe_unload + the `plugin_infos_from_rows` row refresh. The host's
// identifier→SourceKind detach mapping is tested in `pluginhost.rs`, and the
// concrete tab close in `tabs.rs::detach_sources_of_kind_*`.

/// A throwaway native (non-builtin) provider plugin, so a test can add a
/// runtime-loaded-style plugin to the session manager and then unload it.
struct UnloadableProvider {
    manifest: crate::plugin::PluginManifest,
}
impl crate::plugin::Plugin for UnloadableProvider {
    fn manifest(&self) -> &crate::plugin::PluginManifest {
        &self.manifest
    }
    fn contributions(&self) -> Vec<crate::plugin::Contribution> {
        vec![crate::plugin::Contribution::Provider(
            crate::plugin::ProviderSpec::new(
                |_t| true,
                |t| {
                    Ok(
                        std::sync::Arc::new(crate::provider::BufferProvider::new(vec![], t))
                            as crate::plugin::SharedProvider,
                    )
                },
            ),
        )]
    }
}

fn unloadable_native(name: &str) -> Box<dyn crate::plugin::Plugin> {
    let mut m = crate::plugin::PluginManifest::builtin(
        name,
        "a runtime-loaded native provider",
        vec![crate::plugin::Permission::ReadMemory],
    );
    m.kind = crate::plugin::PluginKind::Native;
    m.load = crate::plugin::LoadType::Auto;
    m.dll_file_name = format!("{}.so", name.to_lowercase());
    Box::new(UnloadableProvider { manifest: m })
}

#[test]
fn plugin_info_marks_loaded_native_as_not_builtin() {
    // A loaded native plugin is is_builtin = false → the dialog renders its
    // Unload control (the per-row guard), while built-ins do not.
    let path = temp_settings_path();
    let mut mgr = session_manager_at(&path);
    let id = mgr.add_plugin(unloadable_native("Remote Reader"));
    let infos = crate::ui::dialogs::plugin_manager::plugin_infos_from_rows(mgr.plugins_view());

    let native = infos.iter().find(|i| i.identifier == id).unwrap();
    assert!(!native.is_builtin, "a loaded native plugin is not built-in");
    // Its name is NOT given the built-in "<name> Provider" suffix.
    assert_eq!(native.name, "Remote Reader");
    // The built-ins are still flagged built-in.
    assert!(infos
        .iter()
        .filter(|i| i.identifier != id)
        .all(|i| i.is_builtin));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn safe_unload_through_host_removes_row_and_keeps_builtins() {
    // The exact non-gpui work the dialog's Unload arm does: safe_unload on the
    // session manager (with a host) drops the plugin's row + provider, and the
    // refreshed `plugin_infos_from_rows` no longer lists it while every
    // built-in survives (PARITY: built-ins are never unloaded).
    let path = temp_settings_path();
    let mut mgr = session_manager_at(&path);
    let id = mgr.add_plugin(unloadable_native("Doomed Reader"));

    // Pre-unload: the row + the registered provider are present.
    let before = crate::ui::dialogs::plugin_manager::plugin_infos_from_rows(mgr.plugins_view());
    assert!(before.iter().any(|i| i.identifier == id));
    assert!(mgr.registry().find(&id).is_some());

    // Safe-unload through a host (MockPluginHost stands in for the gpui-bound
    // LivePluginHost; both call detach_documents_using FIRST inside safe_unload).
    let mut host = crate::plugin::MockPluginHost::new();
    assert!(mgr.safe_unload(&id, &mut host));
    // The host WAS asked to detach the unloaded provider's documents first.
    assert_eq!(host.detached(), [id.as_str()]);

    // Post-unload: the row + provider are gone…
    let after = crate::ui::dialogs::plugin_manager::plugin_infos_from_rows(mgr.plugins_view());
    assert!(!after.iter().any(|i| i.identifier == id));
    assert!(mgr.registry().find(&id).is_none());
    // …and every built-in is still listed + still registered (parity).
    for builtin in builtin_ids() {
        assert!(
            after.iter().any(|i| i.identifier == builtin),
            "built-in {builtin} survives the unload"
        );
        assert!(mgr.registry().find(builtin).is_some());
    }
    assert_eq!(mgr.registry().providers().len(), builtin_ids().len());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn view_options_default_matches_cpp_view_menu() {
    // C++ persisted QSettings defaults (main.cpp:1336-1411): compactColumns,
    // treeLines, relativeOffsets, hoverEffects ON; typeHints, showComments,
    // minimap OFF.
    let d = ViewOptions::default();
    assert!(d.compact_columns);
    assert!(d.tree_lines);
    assert!(d.relative_offsets);
    assert!(!d.type_hints);
    assert!(!d.show_comments);
    assert!(d.hover_effects);
    assert!(!d.minimap);
}

#[test]
fn view_option_get_set_round_trips_every_variant() {
    let mut o = ViewOptions::default();
    for opt in ViewOpt::ALL {
        let before = o.get(opt);
        o.set(opt, !before);
        assert_eq!(o.get(opt), !before, "{opt:?} did not flip");
        o.set(opt, before);
        assert_eq!(o.get(opt), before, "{opt:?} did not restore");
    }
}

#[test]
fn view_option_command_ids_match_menu_contract() {
    // The ✓-driving ids must be exactly the MENU CONTRACT [check] ids so
    // MenuBar::set_command_checked targets the right rows.
    assert_eq!(ViewOpt::CompactColumns.command_id(), "view.compact_columns");
    assert_eq!(ViewOpt::TreeLines.command_id(), "view.tree_lines");
    assert_eq!(
        ViewOpt::RelativeOffsets.command_id(),
        "view.relative_offsets"
    );
    assert_eq!(ViewOpt::TypeHints.command_id(), "view.type_hints");
    assert_eq!(ViewOpt::ShowComments.command_id(), "view.comments");
    assert_eq!(ViewOpt::HoverEffects.command_id(), "view.hover");
    assert_eq!(ViewOpt::Minimap.command_id(), "view.minimap");
    // Every variant maps to a distinct id.
    let ids: Vec<&str> = ViewOpt::ALL.iter().map(|o| o.command_id()).collect();
    let mut uniq = ids.clone();
    uniq.sort_unstable();
    uniq.dedup();
    assert_eq!(ids.len(), uniq.len(), "command ids must be unique");
}

#[test]
fn export_kind_extensions_are_sensible() {
    assert_eq!(ExportKind::Cpp.extension(), ".h");
    assert_eq!(ExportKind::Rust.extension(), ".rs");
    assert_eq!(ExportKind::Defines.extension(), ".h");
    assert_eq!(ExportKind::CSharp.extension(), ".cs");
    assert_eq!(ExportKind::Python.extension(), ".py");
    assert_eq!(ExportKind::Xml.extension(), ".xml");
}

/// Build a tree with TWO independent top-level structs (no reference between
/// them) so a `*_tree`/view-root export would only emit ONE while the
/// full-SDK export emits BOTH — the property the C++ `exportToFile`
/// (`renderCodeAll`; main.cpp:5764) guarantees.
fn two_independent_structs() -> (crate::core::NodeTree, u64) {
    use crate::core::{Node, NodeKind, NodeTree};
    let mut tree = NodeTree::new();
    let ai = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "StructA".into(),
        struct_type_name: "StructA".into(),
        parent_id: 0,
        offset: 0,
        ..Node::default()
    });
    let a_id = tree.nodes[ai].id;
    tree.add_node(Node {
        kind: NodeKind::Int32,
        name: "valueA".into(),
        parent_id: a_id,
        offset: 0,
        ..Node::default()
    });
    let bi = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "StructB".into(),
        struct_type_name: "StructB".into(),
        parent_id: 0,
        offset: 0x100,
        ..Node::default()
    });
    let b_id = tree.nodes[bi].id;
    tree.add_node(Node {
        kind: NodeKind::UInt64,
        name: "valueB".into(),
        parent_id: b_id,
        offset: 0,
        ..Node::default()
    });
    (tree, a_id)
}

#[test]
fn export_code_format_maps_kind_to_generator_format() {
    use crate::generator::CodeFormat;
    assert_eq!(ExportKind::Cpp.code_format(), Some(CodeFormat::CppHeader));
    assert_eq!(ExportKind::Rust.code_format(), Some(CodeFormat::RustStruct));
    assert_eq!(
        ExportKind::Defines.code_format(),
        Some(CodeFormat::DefineOffsets)
    );
    assert_eq!(
        ExportKind::CSharp.code_format(),
        Some(CodeFormat::CSharpStruct)
    );
    assert_eq!(
        ExportKind::Python.code_format(),
        Some(CodeFormat::PythonCtypes)
    );
    // XML is not a generator format (routes through the importer's exporter).
    assert_eq!(ExportKind::Xml.code_format(), None);
}

#[test]
fn export_file_filter_matches_code_format_file_filter() {
    // Each code export's filter mirrors the generator's per-format filter
    // (the C++ `codeFormatFileFilter`; main.cpp:5758). XML uses its own.
    for (kind, fmt) in [
        (ExportKind::Cpp, crate::generator::CodeFormat::CppHeader),
        (ExportKind::Rust, crate::generator::CodeFormat::RustStruct),
        (
            ExportKind::Defines,
            crate::generator::CodeFormat::DefineOffsets,
        ),
        (
            ExportKind::CSharp,
            crate::generator::CodeFormat::CSharpStruct,
        ),
        (
            ExportKind::Python,
            crate::generator::CodeFormat::PythonCtypes,
        ),
    ] {
        assert_eq!(
            kind.file_filter(),
            crate::generator::code_format_file_filter(fmt)
        );
    }
    assert!(ExportKind::Xml.file_filter().to_lowercase().contains("xml"));
}

#[test]
fn export_renders_full_sdk_regardless_of_view_root() {
    // The C++ export always calls renderCodeAll — every root struct, ignoring
    // the open view root. So even with a non-zero `a_id` selected, BOTH
    // structs must appear in the output.
    let (tree, _a_id) = two_independent_structs();
    let out = ExportKind::Cpp
        .render(&tree, None, false)
        .expect("non-empty C++ export");
    assert!(out.contains("struct StructA"), "missing StructA:\n{out}");
    assert!(out.contains("struct StructB"), "missing StructB:\n{out}");
}

#[test]
fn export_threads_aliases_and_asserts() {
    use crate::core::NodeKind;
    use crate::generator::TypeAliases;
    let (tree, _a_id) = two_independent_structs();

    // emit_asserts=false → no static_assert; true → present (the persisted
    // generatorAsserts flag, threaded through the export; main.cpp:5763).
    let no_assert = ExportKind::Cpp.render(&tree, None, false).unwrap();
    assert!(!no_assert.contains("static_assert"));
    let with_assert = ExportKind::Cpp.render(&tree, None, true).unwrap();
    assert!(with_assert.contains("static_assert"));

    // type_aliases override the rendered type name (the C++ tab->doc->typeAliases
    // passed to renderCodeAll; main.cpp:5761-5764).
    let mut aliases: TypeAliases = TypeAliases::new();
    aliases.insert(NodeKind::Int32, "LONG".into());
    let aliased = ExportKind::Cpp
        .render(&tree, Some(&aliases), false)
        .unwrap();
    assert!(aliased.contains("LONG"), "alias not applied:\n{aliased}");
}

#[test]
fn export_no_struct_tree_emits_header_but_no_structs() {
    // The C++ `renderCodeAll` (and thus the export) emits the `#pragma once`
    // header even when there are no structs (generator: full_sdk_no_structs),
    // so the C++-header export is non-empty but struct-free — NOT `None`.
    use crate::core::NodeTree;
    let tree = NodeTree::new();
    let out = ExportKind::Cpp
        .render(&tree, None, false)
        .expect("C++ header export is the bare header, not None");
    assert!(out.contains("#pragma once"));
    assert!(!out.contains("struct "));
}

#[test]
fn import_kind_prompts_are_distinct_and_nonempty() {
    let prompts = [
        ImportKind::Source.prompt(),
        ImportKind::Xml.prompt(),
        ImportKind::Pdb.prompt(),
    ];
    for p in prompts {
        assert!(!p.is_empty());
    }
    assert_ne!(prompts[0], prompts[1]);
    assert_ne!(prompts[1], prompts[2]);
}

#[test]
fn root_kind_class_keyword_and_title_are_distinct() {
    // The three New commands must seed distinct root kinds + titles (the bug:
    // all three collapsed into one blank "Untitled").
    assert_eq!(RootKind::Class.class_keyword(), "class");
    assert_eq!(RootKind::Struct.class_keyword(), "struct");
    assert_eq!(RootKind::Enum.class_keyword(), "enum");
    let titles = [
        RootKind::Class.title(),
        RootKind::Struct.title(),
        RootKind::Enum.title(),
    ];
    let mut uniq = titles.to_vec();
    uniq.sort_unstable();
    uniq.dedup();
    assert_eq!(uniq.len(), 3, "each kind must have a distinct tab title");
}

#[test]
fn seed_root_doc_builds_a_root_struct_with_the_kind_keyword() {
    use crate::core::NodeKind;
    for kind in [RootKind::Class, RootKind::Struct, RootKind::Enum] {
        let doc = seed_root_doc(kind);
        // Exactly one top-level struct, carrying the kind's class_keyword.
        let roots: Vec<&crate::core::Node> = doc
            .tree
            .nodes
            .iter()
            .filter(|n| n.parent_id == 0 && n.kind == NodeKind::Struct)
            .collect();
        assert_eq!(roots.len(), 1, "{kind:?} should seed one root struct");
        assert_eq!(roots[0].class_keyword, kind.class_keyword());
        let children: Vec<&crate::core::Node> = doc
            .tree
            .nodes
            .iter()
            .filter(|n| n.parent_id == roots[0].id)
            .collect();
        if matches!(kind, RootKind::Enum) {
            // New Enum: 5 named members, NO hex children (C++ buildEmptyStruct
            // enum branch, main.cpp:3981-4000).
            assert_eq!(children.len(), 0, "enum should seed no hex fields");
            assert_eq!(roots[0].enum_members.len(), 5, "enum should seed 5 members");
        } else {
            // Class/Struct: the 16-field hex body landed under the root, named
            // with 2-digit-min zero-padded hex offsets (field_00..field_78).
            assert_eq!(children.len(), 16, "{kind:?} should seed 16 hex fields");
            assert_eq!(children[0].name, "field_00");
            assert_eq!(children[1].name, "field_08");
            assert!(roots[0].enum_members.is_empty());
        }
        // A sensible default base (the C++ template).
        assert_eq!(doc.tree.base_address, 0x0040_0000);
    }
}

#[test]
fn relabel_command_flips_the_mcp_label() {
    use crate::ui::pickers::commandpalette::{menu_tree_with, MenuNode};
    let mut tree = menu_tree_with(&[], &[]);
    super::relabel_command(&mut tree, "tools.mcp", "Stop MCP Server");
    // Find the relabelled leaf.
    fn find<'a>(nodes: &'a [MenuNode], cmd: &str) -> Option<&'a str> {
        for n in nodes {
            match n {
                MenuNode::Item { label, command, .. } if command == cmd => {
                    return Some(label.as_str())
                }
                MenuNode::Submenu { children, .. } => {
                    if let Some(l) = find(children, cmd) {
                        return Some(l);
                    }
                }
                _ => {}
            }
        }
        None
    }
    #[cfg(feature = "mcp")]
    assert_eq!(find(&tree, "tools.mcp"), Some("Stop MCP Server"));
    #[cfg(not(feature = "mcp"))]
    assert_eq!(find(&tree, "tools.mcp"), None);
}

#[test]
fn editor_text_keys_are_scoped_off_the_inline_field() {
    // The cross-cutting inline-edit fix: bare printable editor accelerators must
    // be re-scoped to `RcxEditor && !RcxFieldInput` so they fall through to the
    // focused field's text input, while modified / navigation accelerators keep
    // their plain `RcxEditor` scope. Import the gpui binding types by name (NOT
    // a `gpui::*` glob — see the module note) to keep test hygiene lean.
    use gpui::{actions, KeyBinding};

    actions!(rcx_test, [PrintableA, NamedSpace, ModifiedD, NavUp]);

    let input = vec![
        KeyBinding::new("t", PrintableA, Some("RcxEditor")), // bare printable → rescoped
        KeyBinding::new("space", NamedSpace, Some("RcxEditor")), // named printable → rescoped
        KeyBinding::new("ctrl-d", ModifiedD, Some("RcxEditor")), // modified → unchanged
        KeyBinding::new("up", NavUp, Some("RcxEditor")),     // navigation → unchanged
    ];
    let out = super::scope_editor_text_keys_to_non_field(input);
    let pred = |b: &KeyBinding| b.predicate().map(|p| p.to_string()).unwrap_or_default();

    assert_eq!(
        pred(&out[0]),
        "RcxEditor && !RcxFieldInput && !RcxFindBar",
        "bare `t` must dodge the field + find bar"
    );
    assert_eq!(
        pred(&out[1]),
        "RcxEditor && !RcxFieldInput && !RcxFindBar",
        "named printable `space` must dodge the field + find bar"
    );
    assert_eq!(
        pred(&out[2]),
        "RcxEditor",
        "`ctrl-d` is not text — left scoped to the editor"
    );
    assert_eq!(
        pred(&out[3]),
        "RcxEditor",
        "`up` navigation is not text — left scoped"
    );

    // The keystroke + action survive the rewrite (only the predicate changes).
    assert_eq!(out[0].keystrokes()[0].inner().key, "t");
}

// ── F3 live declarative-UI host: parity guard + menu injection + Elm loop ──
//
// The gpui mount (PluginPanel/PluginDialog into the dock/modal) follows the
// repo convention of NOT being unit-tested (it needs a live Window); every
// routing DECISION is covered here + in the manager/host modules. These tests
// import the pure helpers + the gpui-free manager/MockPluginHost seam the live
// path mirrors (so they stay headless — no `gpui::*` glob is pulled in).

/// The direct children-commands of the &Plugins submenu, in order (the menu-bar
/// surface the F3 injection targets).
fn plugins_submenu_commands(tree: &[crate::ui::pickers::commandpalette::MenuNode]) -> Vec<String> {
    use crate::ui::pickers::commandpalette::MenuNode;
    for n in tree {
        if let MenuNode::Submenu { label, children } = n {
            if label == "&Plugins" {
                return children
                    .iter()
                    .filter_map(|c| match c {
                        MenuNode::Item { command, .. } => Some(command.clone()),
                        _ => None,
                    })
                    .collect();
            }
        }
    }
    Vec::new()
}

#[test]
fn plugins_menu_is_byte_identical_without_contributions() {
    // PARITY: with no plugin UI contributions, injecting leaves the &Plugins
    // submenu EXACTLY as the static tree built it — only [plugins.manage].
    use crate::ui::pickers::commandpalette::menu_tree_with;
    let mut tree = menu_tree_with(&[], &[]);
    let before = plugins_submenu_commands(&tree);
    assert_eq!(before, ["plugins.manage"]);
    // An empty contributions list is the default-build state.
    super::inject_plugin_menu_items(&mut tree, &[]);
    assert_eq!(plugins_submenu_commands(&tree), ["plugins.manage"]);
}

#[test]
fn plugins_menu_injects_demo_commands_after_manage() {
    // With the demo loaded, its two Menu-slot commands are appended AFTER the
    // static Manage Plugins… row (so the existing row is untouched, and the
    // dialog dialog/panel — not Menu-slot — are NOT injected as menu items).
    use crate::plugin::PluginManager;
    use crate::ui::pickers::commandpalette::menu_tree_with;
    let mgr = PluginManager::with_builtins_and_demo();
    let mut tree = menu_tree_with(&[], &[]);
    super::inject_plugin_menu_items(&mut tree, &mgr.ui_contributions());
    assert_eq!(
        plugins_submenu_commands(&tree),
        [
            "plugins.manage",
            crate::plugin::demo::CMD_PING,
            crate::plugin::demo::CMD_OPEN_TARGET,
        ]
    );
}

#[test]
fn dock_placement_maps_every_side() {
    use crate::plugin::DockSide;
    use gpui_component::dock::DockPlacement;
    assert!(matches!(
        super::dock_placement_for(DockSide::Left),
        DockPlacement::Left
    ));
    assert!(matches!(
        super::dock_placement_for(DockSide::Right),
        DockPlacement::Right
    ));
    assert!(matches!(
        super::dock_placement_for(DockSide::Bottom),
        DockPlacement::Bottom
    ));
}

#[test]
fn demo_command_elm_round_trips_through_manager_and_host() {
    // The exact decisions `dispatch_plugin_command` / `route_plugin_panel_event`
    // make, on the gpui-free seam the live path mirrors: ping toasts;
    // open_target requests a dialog open (these two are the menu-injected
    // commands run_menu_command routes); the panel's Refresh button bumps the
    // counter and returns a fresh tree the panel re-renders.
    use crate::plugin::demo::{CMD_OPEN_TARGET, CMD_PING, CMD_REFRESH, DIALOG_ID, PANEL_ID};
    use crate::plugin::{MockPluginHost, PluginManager, UiEvent, ViewTree};

    let mut mgr = PluginManager::with_builtins_and_demo();
    let mut host = MockPluginHost::new();

    // Only the two Menu-slot commands are plugin commands run_menu_command
    // routes (refresh is a panel button id, not a contributed command).
    assert!(mgr.is_plugin_command(CMD_PING));
    assert!(mgr.is_plugin_command(CMD_OPEN_TARGET));
    assert!(!mgr.is_plugin_command(CMD_REFRESH));

    // ping → a toast surfaces (the live dispatch drains host toasts → notify).
    let res = mgr.handle_command(CMD_PING, serde_json::Value::Null, &mut host);
    assert!(res.handled);
    assert_eq!(host.toasts(), ["Plugin Demo: pong"]);

    // open_target → the host records an open-dialog request (the live dispatch
    // drains open_dialogs → open_plugin_dialog).
    mgr.handle_command(CMD_OPEN_TARGET, serde_json::Value::Null, &mut host);
    assert_eq!(host.opened_dialogs(), [DIALOG_ID]);

    // Panel Refresh button → handle_ui_event returns Some(fresh tree) the live
    // `route_plugin_panel_event` pushes into the mounted PluginPanel; the tree
    // reflects the bumped counter.
    let tree = mgr
        .handle_ui_event(
            PANEL_ID,
            UiEvent::Clicked(CMD_REFRESH.to_string()),
            &mut host,
        )
        .expect("refresh re-renders the panel");
    let ViewTree::Column(children) = &tree else {
        panic!("panel root is a Column");
    };
    assert!(children.iter().any(|c| matches!(c, ViewTree::KeyValue(p)
            if p.iter().any(|(k, v)| k == "Refreshes" && v == "1"))));
    // And view_tree resolves the panel for a request_rerender drain.
    assert!(mgr.view_tree(PANEL_ID).is_some());
}

#[test]
fn demo_dialog_attach_sets_source_and_closes_through_host() {
    // The dialog Ui round-trip: Attach sets the data source, asks to close the
    // dialog (the live `open_plugin_dialog` Ui handler closes the modal on that
    // request), and returns a fresh tree.
    use crate::plugin::demo::{BTN_ATTACH, DEMO_IDENTIFIER, DIALOG_ID};
    use crate::plugin::UiEvent;
    use crate::plugin::{MockPluginHost, PluginManager};

    let mut mgr = PluginManager::with_builtins_and_demo();
    let mut host = MockPluginHost::new();
    let tree = mgr.handle_ui_event(
        DIALOG_ID,
        UiEvent::Clicked(BTN_ATTACH.to_string()),
        &mut host,
    );
    assert!(tree.is_some(), "Attach re-renders the dialog");
    assert_eq!(host.closed_dialogs(), [DIALOG_ID]);
    assert_eq!(
        host.data_source(),
        Some(&(DEMO_IDENTIFIER.to_string(), "1234:notepad.exe".to_string()))
    );
}

// ── A2 GAP 2: ReClass-XML byte-sniff (the C++ project_open probe) ──

#[test]
fn sniff_detects_reclass_xml_signatures() {
    // Port of the C++ `head.trimmed().startsWith("<?xml") ||
    // startsWith("<ReClass")` (main.cpp:6129).
    // A `<?xml …` prolog → XML.
    assert!(sniff_is_reclass_xml(b"<?xml version=\"1.0\"?>"));
    // Leading whitespace is trimmed before the prefix test → still XML.
    assert!(sniff_is_reclass_xml(b"  <ReClass>"));
    assert!(sniff_is_reclass_xml(b"\n\t <?xml"));
    // A JSON document is NOT XML (the native `.rcx` load path).
    assert!(!sniff_is_reclass_xml(b"{\"json\": true}"));
    // Empty / non-matching bytes → not XML.
    assert!(!sniff_is_reclass_xml(b""));
    assert!(!sniff_is_reclass_xml(b"GIF89a"));
}

#[test]
fn path_sniff_chooses_importer_by_content_not_extension() {
    // The headline of the gap: the importer is chosen by the file's BYTES, not
    // its name. An `.xml` holding JSON is NOT XML; a `.rcx` holding XML IS XML.
    let base = temp_settings_path();

    // `<?xml …` true.
    let xml = base.with_extension("rcx_xmlprolog");
    std::fs::write(&xml, b"<?xml version=\"1.0\"?>\n<ReClass>").unwrap();
    assert!(super::MainWindow::path_is_reclass_xml(&xml));

    // Leading-whitespace `<ReClass>` true.
    let ws = base.with_extension("rcx_wsreclass");
    std::fs::write(&ws, b"   <ReClass>\n").unwrap();
    assert!(super::MainWindow::path_is_reclass_xml(&ws));

    // An `.xml`-EXTENSION file carrying JSON bytes → NOT XML (falls through to
    // the native JSON load).
    let xml_ext_json = base.with_extension("xml");
    std::fs::write(&xml_ext_json, b"{\"json\": 1}").unwrap();
    assert!(!super::MainWindow::path_is_reclass_xml(&xml_ext_json));

    // A `.rcx`-EXTENSION file carrying XML bytes → XML (the importer chosen by
    // signature, not by name).
    let rcx_ext_xml = base.with_extension("rcx");
    std::fs::write(&rcx_ext_xml, b"<?xml version=\"1.0\"?>").unwrap();
    assert!(super::MainWindow::path_is_reclass_xml(&rcx_ext_xml));

    // A missing file → false (the C++ leaves isXml=false when the probe fails
    // to open).
    let missing = base.with_extension("does_not_exist");
    assert!(!super::MainWindow::path_is_reclass_xml(&missing));

    for p in [&xml, &ws, &xml_ext_json, &rcx_ext_xml] {
        let _ = std::fs::remove_file(p);
    }
}

// ── A2 GAP 4: recent-file age from mtime (the C++ buildGroups bucketing) ──

#[test]
fn age_days_computed_from_known_timestamp() {
    // `age_days_from_secs` is the per-recent-file age `recent_entries` now feeds
    // into the start-page buckets. Assert the day-delta and the resulting
    // bucket for known timestamps.
    use crate::ui::chrome::startpage::{age_days_from_secs, bucket_for, Bucket, RecentEntry};
    const DAY: u64 = 24 * 60 * 60;
    let now = 1_000 * DAY; // an arbitrary fixed "now" in whole days.

    // Same day → 0 (Today).
    assert_eq!(age_days_from_secs(now, now), 0);
    // 1 day ago → Yesterday.
    assert_eq!(age_days_from_secs(now, now - DAY), 1);
    // 3 days ago → This Week.
    assert_eq!(age_days_from_secs(now, now - 3 * DAY), 3);
    // 40 days ago → Older.
    assert_eq!(age_days_from_secs(now, now - 40 * DAY), 40);
    // A future mtime (clock skew) floors at 0.
    assert_eq!(age_days_from_secs(now, now + 5 * DAY), 0);

    // The computed age drives the bucket the start page files the row under.
    let entry_for = |age: i64| RecentEntry {
        path: "/p/x.rcx".into(),
        file_name: "x.rcx".into(),
        dir_path: "/p".into(),
        age_days: age,
        is_example: false,
    };
    assert_eq!(bucket_for(&entry_for(0)), Bucket::Today);
    assert_eq!(bucket_for(&entry_for(1)), Bucket::Yesterday);
    assert_eq!(bucket_for(&entry_for(3)), Bucket::ThisWeek);
    assert_eq!(bucket_for(&entry_for(15)), Bucket::ThisMonth);
    assert_eq!(bucket_for(&entry_for(40)), Bucket::Older);
}
