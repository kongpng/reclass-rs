//! `PluginHost` — the controlled callback surface a plugin uses to talk back to
//! the application (design §2).
//!
//! Design §2: "`PluginHost` is the controlled callback surface (read the
//! document/tree, add nodes, set the data source, read provider bytes, resolve a
//! symbol, show a toast, open a `Dialog`, persist plugin settings)." Phase 1 ships
//! the **minimal-but-extensible** subset the built-ins + the conformance suite
//! need; the richer document-mutation hooks (`add_node`, `set_data_source`) are
//! present as documented **Phase-2 expansion points** with default no-ops, so the
//! trait is usable now and grows without breaking implementors.
//!
//! A [`MockPluginHost`] is always compiled (not test-gated) so it backs both the
//! `#[cfg(test)]` tests here and the future plugin conformance suite (design §G —
//! "Mock `PluginHost` + a conformance test-suite a plugin can run against").

use std::collections::BTreeMap;

/// The controlled host callback surface (design §2). Object-safe: plugins receive
/// `&mut dyn PluginHost`.
///
/// The split: **read** methods take `&self`, **mutating/effectful** methods take
/// `&mut self`. The document-mutation hooks ([`add_node`](PluginHost::add_node),
/// [`set_data_source`](PluginHost::set_data_source)) default to no-ops in Phase 1
/// and are wired to the real controller in Phase 2.
pub trait PluginHost {
    /// Read bytes from the active provider into `buf` (design §2 "read provider
    /// bytes"; the C++ provider `read(addr,buf,len)`). Returns success.
    fn read_provider(&self, addr: u64, buf: &mut [u8]) -> bool;

    /// The active provider's display name (C++ `Provider::name()`).
    fn provider_name(&self) -> String;

    /// Show a short transient message (design §2 "show a toast"; design §7.A
    /// [fix] — surfaced feedback instead of console logging).
    fn show_toast(&mut self, msg: &str);

    /// Request the host open a contributed `Dialog` by id (design §2/§3 — the
    /// generalized C++ `selectTarget`).
    fn open_dialog(&mut self, id: &str);

    /// Read a persisted per-plugin setting (design §2 "persist plugin settings";
    /// design §7.A [fix] — C++ persists nothing).
    fn get_setting(&self, key: &str) -> Option<String>;

    /// Persist a per-plugin setting.
    fn set_setting(&mut self, key: &str, val: &str);

    // ── Phase-2 expansion points (default no-ops; design §2 document hooks) ──

    /// Add a node to the active document tree at `parent_path` (design §2 "add
    /// nodes"). Phase-1 default: no-op (the real wiring to the controller's tree
    /// mutation API lands in Phase 2). Returns whether the host accepted it.
    fn add_node(&mut self, _parent_path: &str, _node_kind: &str) -> bool {
        false
    }

    /// Switch the active document's data source to the provider `identifier`
    /// (design §2 "set the data source"; the C++ `selectSource`/`attachViaPlugin`
    /// path). Phase-1 default: no-op. Returns whether the host accepted it.
    fn set_data_source(&mut self, _identifier: &str, _target: &str) -> bool {
        false
    }

    /// Close a contributed `Dialog` by id (design §3 — the inverse of
    /// [`open_dialog`](PluginHost::open_dialog)). A plugin's
    /// [`handle_ui_event`](crate::plugin::contract::Plugin::handle_ui_event)
    /// calls this when its dialog's Attach/OK/Cancel finishes the interaction.
    /// Default no-op so existing host implementors don't break; the Phase-2
    /// declarative host wires it to dismiss the modal.
    fn close_dialog(&mut self, _id: &str) {}

    /// Ask the host to re-render the contributed view `view` (a `Panel`/`Dialog`/
    /// `StatusItem` id) by pulling a fresh `ViewTree` from the plugin (design §3
    /// Elm loop). A command handler that mutated panel state calls this so the
    /// already-mounted panel refreshes without a UI event having driven it.
    /// Default no-op; the Phase-2 host re-asks the plugin for the view's tree.
    fn request_rerender(&mut self, _view: &str) {}

    /// Detach every open document whose data source is the provider `identifier`,
    /// returning how many were detached (design §7.A [fix] — **safe unload**).
    ///
    /// The C++ unloads a plugin's backing library while a document may still hold
    /// that provider — a dangling-pointer crash it only *warns* about
    /// (cpp_reference §2, §10.3). The Phase-6
    /// [`PluginManager::safe_unload`](crate::plugin::manager::PluginManager::safe_unload)
    /// calls this **first** so no document outlives the provider it points at.
    /// Default no-op returning `0` so no existing host implementor breaks; the
    /// real host detaches the affected tabs.
    fn detach_documents_using(&mut self, _identifier: &str) -> usize {
        0
    }
}

/// An always-compiled in-memory [`PluginHost`] capturing side effects, for tests
/// and the plugin conformance suite (design §G). Holds an optional byte buffer the
/// `read_provider` calls slice (so a provider-driven test can supply bytes).
#[derive(Clone, Debug, Default)]
pub struct MockPluginHost {
    provider_bytes: Vec<u8>,
    provider_name: String,
    toasts: Vec<String>,
    opened_dialogs: Vec<String>,
    settings: BTreeMap<String, String>,
    added_nodes: Vec<(String, String)>,
    data_source: Option<(String, String)>,
    closed_dialogs: Vec<String>,
    rerender_requests: Vec<String>,
    detached: Vec<String>,
}

impl MockPluginHost {
    /// An empty mock (no provider bytes).
    pub fn new() -> Self {
        MockPluginHost::default()
    }

    /// A mock whose `read_provider` slices `bytes` (addr is the offset) and whose
    /// `provider_name` returns `name`.
    pub fn with_provider(bytes: Vec<u8>, name: impl Into<String>) -> Self {
        MockPluginHost {
            provider_bytes: bytes,
            provider_name: name.into(),
            ..MockPluginHost::default()
        }
    }

    /// The toasts shown so far, in order (`show_toast`).
    pub fn toasts(&self) -> &[String] {
        &self.toasts
    }

    /// The dialog ids opened so far, in order (`open_dialog`).
    pub fn opened_dialogs(&self) -> &[String] {
        &self.opened_dialogs
    }

    /// The `(parent_path, node_kind)` pairs passed to `add_node`.
    pub fn added_nodes(&self) -> &[(String, String)] {
        &self.added_nodes
    }

    /// The last `(identifier, target)` passed to `set_data_source`, if any.
    pub fn data_source(&self) -> Option<&(String, String)> {
        self.data_source.as_ref()
    }

    /// The dialog ids passed to `close_dialog`, in order.
    pub fn closed_dialogs(&self) -> &[String] {
        &self.closed_dialogs
    }

    /// The view ids passed to `request_rerender`, in order.
    pub fn rerender_requests(&self) -> &[String] {
        &self.rerender_requests
    }

    /// The provider identifiers passed to `detach_documents_using`, in order —
    /// so a `safe_unload` / conformance test can assert the detach happened
    /// **before** the plugin was dropped (design §7.A [fix] safe-unload).
    pub fn detached(&self) -> &[String] {
        &self.detached
    }
}

impl PluginHost for MockPluginHost {
    fn read_provider(&self, addr: u64, buf: &mut [u8]) -> bool {
        let start = addr as usize;
        let end = match start.checked_add(buf.len()) {
            Some(e) => e,
            None => return false,
        };
        if end > self.provider_bytes.len() {
            return false;
        }
        buf.copy_from_slice(&self.provider_bytes[start..end]);
        true
    }

    fn provider_name(&self) -> String {
        self.provider_name.clone()
    }

    fn show_toast(&mut self, msg: &str) {
        self.toasts.push(msg.to_string());
    }

    fn open_dialog(&mut self, id: &str) {
        self.opened_dialogs.push(id.to_string());
    }

    fn get_setting(&self, key: &str) -> Option<String> {
        self.settings.get(key).cloned()
    }

    fn set_setting(&mut self, key: &str, val: &str) {
        self.settings.insert(key.to_string(), val.to_string());
    }

    // Override the Phase-2 expansion points so the mock can *record* them for the
    // conformance suite (the real host wires them to the controller in Phase 2).
    fn add_node(&mut self, parent_path: &str, node_kind: &str) -> bool {
        self.added_nodes
            .push((parent_path.to_string(), node_kind.to_string()));
        true
    }

    fn set_data_source(&mut self, identifier: &str, target: &str) -> bool {
        self.data_source = Some((identifier.to_string(), target.to_string()));
        true
    }

    fn close_dialog(&mut self, id: &str) {
        self.closed_dialogs.push(id.to_string());
    }

    fn request_rerender(&mut self, view: &str) {
        self.rerender_requests.push(view.to_string());
    }

    fn detach_documents_using(&mut self, identifier: &str) -> usize {
        // Record the call so a safe-unload test can assert the host was asked to
        // detach the provider's documents first. The mock pretends one document
        // referenced it (a nonzero count exercises the manager's bookkeeping).
        self.detached.push(identifier.to_string());
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_toasts_dialogs_and_settings() {
        let mut host = MockPluginHost::new();
        host.show_toast("first");
        host.show_toast("second");
        host.open_dialog("pick-target");
        host.set_setting("dll", "/path/x.dll");

        assert_eq!(host.toasts(), ["first", "second"]);
        assert_eq!(host.opened_dialogs(), ["pick-target"]);
        assert_eq!(host.get_setting("dll").as_deref(), Some("/path/x.dll"));
        assert_eq!(host.get_setting("missing"), None);
    }

    #[test]
    fn read_provider_slices_supplied_bytes() {
        let host = MockPluginHost::with_provider(vec![10, 20, 30, 40], "dump.bin");
        assert_eq!(host.provider_name(), "dump.bin");
        let mut buf = [0u8; 2];
        assert!(host.read_provider(1, &mut buf));
        assert_eq!(buf, [20, 30]);
        // Out of range fails (no panic / no overflow).
        assert!(!host.read_provider(3, &mut [0u8; 2]));
        assert!(!host.read_provider(u64::MAX, &mut [0u8; 1]));
    }

    #[test]
    fn phase2_expansion_points_record_in_mock() {
        let mut host = MockPluginHost::new();
        assert!(host.add_node("/root", " Int32"));
        assert!(host.set_data_source("processmemory", "1234:notepad.exe"));
        assert_eq!(
            host.added_nodes(),
            [("/root".to_string(), " Int32".to_string())]
        );
        assert_eq!(
            host.data_source(),
            Some(&("processmemory".to_string(), "1234:notepad.exe".to_string()))
        );
    }

    #[test]
    fn records_close_dialog_and_rerender() {
        let mut host = MockPluginHost::new();
        // Default no-ops are overridden in the mock to record for the conformance
        // suite (the real host wires them to the modal/dock in Phase 2).
        host.close_dialog("demo.target");
        host.request_rerender("demo.panel");
        host.request_rerender("demo.panel");
        assert_eq!(host.closed_dialogs(), ["demo.target"]);
        assert_eq!(host.rerender_requests(), ["demo.panel", "demo.panel"]);
        // Untouched on a fresh mock.
        let fresh = MockPluginHost::new();
        assert!(fresh.closed_dialogs().is_empty());
        assert!(fresh.rerender_requests().is_empty());
    }

    #[test]
    fn close_and_rerender_via_trait_object() {
        let mut host = MockPluginHost::new();
        let dyn_host: &mut dyn PluginHost = &mut host;
        dyn_host.close_dialog("d");
        dyn_host.request_rerender("v");
        assert_eq!(host.closed_dialogs(), ["d"]);
        assert_eq!(host.rerender_requests(), ["v"]);
    }

    #[test]
    fn mock_records_detach_calls() {
        let mut host = MockPluginHost::new();
        // The mock pretends one document referenced each provider.
        assert_eq!(host.detach_documents_using("remoteprocessmemory"), 1);
        assert_eq!(host.detach_documents_using("file"), 1);
        assert_eq!(host.detached(), ["remoteprocessmemory", "file"]);
    }

    #[test]
    fn fresh_mock_records_no_detach() {
        let host = MockPluginHost::new();
        assert!(host.detached().is_empty());
    }

    #[test]
    fn detach_callable_via_trait_object() {
        let mut host = MockPluginHost::new();
        let dyn_host: &mut dyn PluginHost = &mut host;
        let n = dyn_host.detach_documents_using("processmemory");
        assert_eq!(n, 1);
        assert_eq!(host.detached(), ["processmemory"]);
    }

    #[test]
    fn usable_as_trait_object() {
        let mut host = MockPluginHost::new();
        let dyn_host: &mut dyn PluginHost = &mut host;
        dyn_host.show_toast("via dyn");
        dyn_host.set_setting("k", "v");
        assert_eq!(dyn_host.get_setting("k").as_deref(), Some("v"));
        assert_eq!(host.toasts(), ["via dyn"]);
    }
}
