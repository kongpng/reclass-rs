//! `loader` — the **native dynamic loader** (design §6 Phase 3): load a
//! `kind = native` plugin `.so`/`.dll`/`.dylib` via `abi_stable`'s root-module
//! export, with the built-in load-time **version + layout check** that refuses a
//! mismatched plugin (design §7.B [+], strictly safer than C++'s raw `dlopen`).
//!
//! A loaded plugin's `Plugin_TO` is adapted into a `Box<dyn Plugin>` so the
//! EXISTING [`PluginManager::add_plugin`](crate::plugin::manager::PluginManager::add_plugin)
//! registration flow is reused unchanged — the §1 parity table satisfied by a real
//! loaded library, with no change to the contract or the manager (design §6
//! Phase 3 deliverable).
//!
//! Gated behind the `plugins` cargo feature: this module only exists when the
//! stable-ABI deps (`abi_stable` + `reclass-plugin-abi`) are pulled, so default
//! app builds are unaffected (design §2/§6).

use std::path::Path;
use std::sync::Arc;

use abi_stable::library::{lib_header_from_path, LibraryError};
use abi_stable::std_types::{RBox, ROption, RResult, RSlice, RSliceMut, RString, RVec, Tuple2};
use reclass_plugin_abi::{
    AbiCommandResult, AbiContribution, AbiDialogResult, AbiManifest, AbiUiEvent, AbiViewTree,
    PluginHost_TO, PluginHost_TO_TO, PluginModRef, Plugin_TO_TO, Provider_TO_TO,
};

use crate::plugin::contract::{
    CommandResult, CommandSlot, Contribution, DialogResult, DockSide, Plugin, ProcessInfo,
};
use crate::plugin::host::PluginHost;
use crate::plugin::manifest::{LoadType, Permission, PluginKind, PluginManifest};
use crate::plugin::provider_spec::{ProviderSpec, SharedProvider};
use crate::plugin::view::{TreeNode, UiEvent, ViewTree};
use crate::provider::{MemoryRegion, Provider, RegionType};

/// A loaded native plugin error, surfaced to the dialog/toast with detail
/// (design §7.A [fix] — not a "check the console" box).
#[derive(Debug)]
pub enum LoadError {
    /// `abi_stable` refused or failed to load the library (missing export,
    /// version/layout mismatch, dlopen failure, …).
    Abi(LibraryError),
    /// A ReClass.NET **native** compat bridge failed to load (the discovery
    /// sniffer routed the library here, but opening / resolving / validating the
    /// 8 CoreFunctions failed — design §6 Phase 4, §7.A [fix]). Carries the
    /// surfaced reason from
    /// [`load_reclassnet_native`](crate::plugin::reclassnet::load_reclassnet_native).
    RcNet(String),
    /// A ReClass.NET **managed (.NET)** compat load did not produce a provider
    /// (design §6 Phase 5). Carries the surfaced reason from
    /// [`load_reclassnet_managed`](crate::plugin::reclassnet::load_reclassnet_managed):
    /// a node-type / UI plugin skip ("ReClass.NET node-type plugin unsupported" —
    /// the decided memory-backends-only scope, design §8), a missing
    /// `RcNetBridge.dll`, a CLR-unavailable / FW4-missing message, or — on
    /// non-Windows — the Windows-only stub. A benign skip the host surfaces, not a
    /// crash.
    RcNetManaged(String),
    /// A library matched no known plugin format: it is neither our `abi_stable`
    /// root-module plugin nor a ReClass.NET native plugin (the 8 CoreFunctions).
    /// Recorded by the discovery sniffer so the host can surface "not a plugin"
    /// with the underlying reason (design §4 discovery, §7.A [fix]).
    Unrecognized(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Abi(e) => write!(f, "native plugin load failed: {e}"),
            LoadError::RcNet(e) => write!(f, "ReClass.NET native compat load failed: {e}"),
            LoadError::RcNetManaged(e) => write!(f, "ReClass.NET managed compat skipped: {e}"),
            LoadError::Unrecognized(e) => write!(f, "not a recognized plugin: {e}"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<LibraryError> for LoadError {
    fn from(e: LibraryError) -> Self {
        LoadError::Abi(e)
    }
}

/// Load a native plugin `.so`/`.dll`/`.dylib` and adapt it into a
/// `Box<dyn Plugin>` ready for [`PluginManager::add_plugin`] (design §6 Phase 3).
///
/// `dll_file_name` is recorded on the manifest (the C++ `dllFileName` dedupe key /
/// source-picker hint). The library is loaded with `abi_stable`'s version + layout
/// check; on success its root module statics stay alive for the process lifetime
/// (so the returned trait objects' `RBox`es remain valid).
pub fn load_native_plugin(path: &Path) -> Result<Box<dyn Plugin>, LoadError> {
    // NB: we deliberately do NOT use `PluginModRef::load_from_file`, which caches a
    // SINGLE root module per `PluginModRef` type for the whole process — loading a
    // second plugin would silently return the first. `lib_header_from_path` loads
    // (and leaks, so the module stays valid) each library independently, and
    // `init_root_module` performs the same version + layout check (design §7.B
    // [+]) while returning that library's own module. This is what lets the host
    // load multiple native plugins (the C++ folder scan loads many).
    let header = lib_header_from_path(path)?;
    let module: PluginModRef = header.init_root_module()?;
    let dll_file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    let plugin = (module.new_plugin())();
    Ok(Box::new(LoadedPlugin {
        module,
        plugin,
        dll_file_name,
        manifest_once: std::sync::OnceLock::new(),
    }))
}

/// A native plugin loaded across the ABI, presented to the host as a plain
/// `dyn Plugin`. Holds the root module (so provider creation can call back into
/// the loaded library's `create_provider`/`can_handle`/`enumerate_processes`
/// exports) and the `Plugin_TO` instance for the lifecycle/UI methods.
struct LoadedPlugin {
    module: PluginModRef,
    plugin: Plugin_TO_TO<'static, RBox<()>>,
    dll_file_name: String,
    /// Lazily-cached manifest so [`Plugin::manifest`] can return a stable `&`
    /// (the ABI gives an owned `AbiManifest` per call; the manifest is static
    /// plugin metadata, so caching the first conversion is correct).
    manifest_once: std::sync::OnceLock<PluginManifest>,
}

// The loaded trait objects are `Send + Sync` (the sabi traits declare it) and the
// module ref is a static prefix-type ref; the struct is shareable like any other
// `Box<dyn Plugin>` the manager holds.
unsafe impl Send for LoadedPlugin {}
unsafe impl Sync for LoadedPlugin {}

impl Plugin for LoadedPlugin {
    fn manifest(&self) -> &PluginManifest {
        // The ABI gives an owned `AbiManifest` per call; cache the first
        // conversion so we can hand out a stable `&` (the manifest is static
        // plugin metadata).
        self.manifest_once
            .get_or_init(|| from_abi_manifest(self.plugin.manifest(), self.dll_file_name.clone()))
    }

    fn contributions(&self) -> Vec<Contribution> {
        from_abi_contributions(self.plugin.contributions(), self.module)
    }

    fn activate(&mut self, host: &mut dyn PluginHost) {
        let mut bridge = host_bridge(host);
        self.plugin.activate(&mut bridge);
    }

    fn deactivate(&mut self) {
        self.plugin.deactivate();
    }

    fn handle_command(
        &mut self,
        id: &str,
        args: serde_json::Value,
        host: &mut dyn PluginHost,
    ) -> CommandResult {
        let mut bridge = host_bridge(host);
        let args_json = args.to_string();
        let r =
            self.plugin
                .handle_command(RString::from(id), RString::from(args_json), &mut bridge);
        from_abi_command_result(r)
    }

    fn handle_ui_event(
        &mut self,
        view: &str,
        ev: UiEvent,
        host: &mut dyn PluginHost,
    ) -> Option<ViewTree> {
        let mut bridge = host_bridge(host);
        let r = self
            .plugin
            .handle_ui_event(RString::from(view), to_abi_ui_event(ev), &mut bridge);
        match r {
            ROption::RSome(t) => Some(from_abi_view_tree(t)),
            ROption::RNone => None,
        }
    }

    fn handle_dialog_closed(
        &mut self,
        view: &str,
        result: DialogResult,
        host: &mut dyn PluginHost,
    ) -> CommandResult {
        let mut bridge = host_bridge(host);
        let r = self.plugin.handle_dialog_closed(
            RString::from(view),
            to_abi_dialog_result(result),
            &mut bridge,
        );
        from_abi_command_result(r)
    }
}

// ── ABI → host conversions ───────────────────────────────────────────────────

fn from_abi_manifest(m: AbiManifest, dll_file_name: String) -> PluginManifest {
    let permissions = m
        .permissions
        .iter()
        .filter_map(|p| match p.as_str() {
            "read_memory" => Some(Permission::ReadMemory),
            "write_memory" => Some(Permission::WriteMemory),
            "network" => Some(Permission::Network),
            "filesystem" => Some(Permission::Filesystem),
            "add_ui" => Some(Permission::AddUi),
            "add_provider" => Some(Permission::AddProvider),
            _ => None,
        })
        .collect();
    let kind = match m.kind.as_str() {
        "reclassnet" => PluginKind::ReclassNet,
        "process" => PluginKind::Process,
        "builtin" => PluginKind::Builtin,
        _ => PluginKind::Native,
    };
    PluginManifest {
        name: m.name.into(),
        version: m.version.into(),
        author: m.author.into(),
        description: m.description.into(),
        kind,
        load: LoadType::Auto,
        permissions,
        dll_file_name,
    }
}

fn from_abi_command_result(r: AbiCommandResult) -> CommandResult {
    CommandResult {
        handled: r.handled,
        toast: match r.toast {
            ROption::RSome(s) => Some(s.into()),
            ROption::RNone => None,
        },
    }
}

fn from_abi_command_slot(s: reclass_plugin_abi::AbiCommandSlot) -> CommandSlot {
    use reclass_plugin_abi::AbiCommandSlot as A;
    match s {
        A::Menu => CommandSlot::Menu,
        A::EditorContext => CommandSlot::EditorContext,
        A::SourceMenu => CommandSlot::SourceMenu,
        A::Toolbar => CommandSlot::Toolbar,
        A::Palette => CommandSlot::Palette,
    }
}

fn from_abi_dock_side(d: reclass_plugin_abi::AbiDockSide) -> DockSide {
    use reclass_plugin_abi::AbiDockSide as A;
    match d {
        A::Left => DockSide::Left,
        A::Right => DockSide::Right,
        A::Bottom => DockSide::Bottom,
    }
}

fn from_abi_tree_node(n: reclass_plugin_abi::AbiTreeNode) -> TreeNode {
    TreeNode {
        id: n.id.into(),
        label: n.label.into(),
        children: n.children.into_iter().map(from_abi_tree_node).collect(),
    }
}

fn from_abi_view_tree(t: AbiViewTree) -> ViewTree {
    match t {
        AbiViewTree::Column(cs) => {
            ViewTree::Column(cs.into_iter().map(from_abi_view_tree).collect())
        }
        AbiViewTree::Row(cs) => ViewTree::Row(cs.into_iter().map(from_abi_view_tree).collect()),
        AbiViewTree::Group { title, child } => ViewTree::Group {
            title: title.into(),
            child: Box::new(from_abi_view_tree(RBox::into_inner(child))),
        },
        AbiViewTree::Label(s) => ViewTree::Label(s.into()),
        AbiViewTree::Button { id, label } => ViewTree::Button {
            id: id.into(),
            label: label.into(),
        },
        AbiViewTree::TextInput {
            id,
            value,
            placeholder,
        } => ViewTree::TextInput {
            id: id.into(),
            value: value.into(),
            placeholder: placeholder.into(),
        },
        AbiViewTree::Checkbox { id, label, checked } => ViewTree::Checkbox {
            id: id.into(),
            label: label.into(),
            checked,
        },
        AbiViewTree::Dropdown {
            id,
            options,
            selected,
        } => ViewTree::Dropdown {
            id: id.into(),
            options: options.into_iter().map(Into::into).collect(),
            selected: selected as usize,
        },
        AbiViewTree::Table { id, columns, rows } => ViewTree::Table {
            id: id.into(),
            columns: columns.into_iter().map(Into::into).collect(),
            rows: rows
                .into_iter()
                .map(|r| r.into_iter().map(Into::into).collect())
                .collect(),
        },
        AbiViewTree::Tree { id, nodes } => ViewTree::Tree {
            id: id.into(),
            nodes: nodes.into_iter().map(from_abi_tree_node).collect(),
        },
        AbiViewTree::KeyValue(kvs) => ViewTree::KeyValue(
            kvs.into_iter()
                .map(|Tuple2(k, v)| (k.into(), v.into()))
                .collect(),
        ),
        AbiViewTree::Separator => ViewTree::Separator,
    }
}

fn from_abi_contributions(cs: RVec<AbiContribution>, module: PluginModRef) -> Vec<Contribution> {
    cs.into_iter()
        .map(|c| match c {
            AbiContribution::Provider(_spec) => {
                // The provider factory lives in the loaded root module's
                // `create_provider`/`can_handle`/`enumerate_processes` exports.
                // We build a real, module-backed `ProviderSpec` (design §8) so the
                // EXISTING manager registration + attach path works unchanged on a
                // loaded `.so`.
                Contribution::Provider(provider_spec_from_module(module))
            }
            AbiContribution::Command { id, title, slot } => Contribution::Command {
                id: id.into(),
                title: title.into(),
                slot: from_abi_command_slot(slot),
            },
            AbiContribution::Panel {
                id,
                title,
                dock,
                initial,
            } => Contribution::Panel {
                id: id.into(),
                title: title.into(),
                dock: from_abi_dock_side(dock),
                initial: from_abi_view_tree(initial),
            },
            AbiContribution::Dialog { id, title, initial } => Contribution::Dialog {
                id: id.into(),
                title: title.into(),
                initial: from_abi_view_tree(initial),
            },
            AbiContribution::StatusItem { id, initial } => Contribution::StatusItem {
                id: id.into(),
                initial: from_abi_view_tree(initial),
            },
        })
        .collect()
}

/// Build the provider spec backed by a loaded module's exports (design §8 — the
/// provider crosses as `Provider_TO`, so `read()` is a direct native call). The
/// closures capture the module's `create_provider`/`can_handle`/
/// `enumerate_processes` `extern "C"` fn pointers, so attaching this provider runs
/// the loaded library's factory.
fn provider_spec_from_module(module: PluginModRef) -> ProviderSpec {
    let can_handle_fn = module.can_handle();
    let create_fn = module.create_provider();
    let enumerate_fn = module.enumerate_processes();

    let provides_list = !(enumerate_fn)().is_empty();

    let spec = ProviderSpec::new(
        move |target: &str| (can_handle_fn)(RString::from(target)),
        move |target: &str| match (create_fn)(RString::from(target)) {
            RResult::ROk(to) => Ok(Arc::new(LoadedProvider {
                inner: std::sync::Mutex::new(to),
            }) as SharedProvider),
            RResult::RErr(e) => Err(e.into()),
        },
    );
    if provides_list {
        spec.with_enumerate(move || {
            let procs = (enumerate_fn)();
            if procs.is_empty() {
                None
            } else {
                Some(
                    procs
                        .into_iter()
                        .map(|p| ProcessInfo {
                            pid: p.pid,
                            name: p.name.into(),
                            path: p.path.into(),
                            is_32bit: p.is_32bit,
                        })
                        .collect(),
                )
            }
        })
    } else {
        spec
    }
}

// ── host → ABI conversions ───────────────────────────────────────────────────

fn to_abi_ui_event(ev: UiEvent) -> AbiUiEvent {
    match ev {
        UiEvent::Clicked(s) => AbiUiEvent::Clicked(s.into()),
        UiEvent::Submitted { id, text } => AbiUiEvent::Submitted {
            id: id.into(),
            text: text.into(),
        },
        UiEvent::Toggled { id, on } => AbiUiEvent::Toggled { id: id.into(), on },
        UiEvent::RowSelected { table, row } => AbiUiEvent::RowSelected {
            table: table.into(),
            row: row as u64,
        },
    }
}

fn to_abi_dialog_result(r: DialogResult) -> AbiDialogResult {
    match r {
        DialogResult::Submitted { values } => AbiDialogResult::Submitted {
            values: values
                .into_iter()
                .map(|(k, v)| Tuple2(k.into(), v.into()))
                .collect(),
        },
        DialogResult::Cancelled => AbiDialogResult::Cancelled,
    }
}

// ── The loaded provider — wraps a `Provider_TO` as a host `Provider` ──────────

/// A provider obtained from a loaded native plugin (design §8). Its `read()` is a
/// direct native vtable call into the plugin (no per-call marshalling beyond the
/// `RSliceMut` view).
struct LoadedProvider {
    // The ABI `Provider_TO::write` is `&mut self`, but the host `Provider::write`
    // is `&self` (interior mutability — PORTING_providers §5), so the native
    // handle lives behind a `Mutex` and every call locks it. (This crate is off
    // by default; the lock is uncontended in practice.)
    inner: std::sync::Mutex<Provider_TO_TO<'static, RBox<()>>>,
}

unsafe impl Send for LoadedProvider {}
unsafe impl Sync for LoadedProvider {}

impl Provider for LoadedProvider {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        self.inner
            .lock()
            .unwrap()
            .read(addr, RSliceMut::from_mut_slice(buf))
    }
    fn size(&self) -> i32 {
        self.inner.lock().unwrap().size()
    }
    fn name(&self) -> String {
        self.inner.lock().unwrap().name().into()
    }
    fn kind(&self) -> String {
        self.inner.lock().unwrap().kind().into()
    }
    fn base(&self) -> u64 {
        self.inner.lock().unwrap().base()
    }
    fn pointer_size(&self) -> i32 {
        self.inner.lock().unwrap().pointer_size()
    }
    fn is_live(&self) -> bool {
        self.inner.lock().unwrap().is_live()
    }
    fn is_writable(&self) -> bool {
        self.inner.lock().unwrap().is_writable()
    }
    fn write(&self, addr: u64, data: &[u8]) -> bool {
        self.inner
            .lock()
            .unwrap()
            .write(addr, RSlice::from_slice(data))
    }
    fn enumerate_regions(&self) -> Vec<MemoryRegion> {
        self.inner
            .lock()
            .unwrap()
            .enumerate_regions()
            .into_iter()
            .map(|r| MemoryRegion {
                base: r.base,
                size: r.size,
                readable: r.readable,
                writable: r.writable,
                executable: r.executable,
                module_name: r.module_name.into(),
                region_type: match r.region_type {
                    0 => RegionType::Image,
                    1 => RegionType::Mapped,
                    _ => RegionType::Private,
                },
            })
            .collect()
    }
}

// ── The host bridge — presents a host `&mut dyn PluginHost` as `PluginHost_TO` ──

/// Bridges the host's `&mut dyn PluginHost` to the ABI `PluginHost_TO` so a loaded
/// plugin's `activate`/`handle_*` can call back into the host. The raw pointer is
/// valid only for the duration of one method call (the plugin never stores the
/// host beyond the call).
struct HostBridge<'a> {
    host: &'a mut dyn PluginHost,
}

impl PluginHost_TO for HostBridge<'_> {
    fn read_provider(&self, addr: u64, mut buf: RSliceMut<'_, u8>) -> bool {
        self.host.read_provider(addr, buf.as_mut_slice())
    }
    fn provider_name(&self) -> RString {
        self.host.provider_name().into()
    }
    fn show_toast(&mut self, msg: RString) {
        self.host.show_toast(msg.as_str());
    }
    fn open_dialog(&mut self, id: RString) {
        self.host.open_dialog(id.as_str());
    }
    fn close_dialog(&mut self, id: RString) {
        self.host.close_dialog(id.as_str());
    }
    fn get_setting(&self, key: RString) -> ROption<RString> {
        match self.host.get_setting(key.as_str()) {
            Some(v) => ROption::RSome(v.into()),
            None => ROption::RNone,
        }
    }
    fn set_setting(&mut self, key: RString, val: RString) {
        self.host.set_setting(key.as_str(), val.as_str());
    }
    fn add_node(&mut self, parent_path: RString, node_kind: RString) -> bool {
        self.host.add_node(parent_path.as_str(), node_kind.as_str())
    }
    fn set_data_source(&mut self, identifier: RString, target: RString) -> bool {
        self.host
            .set_data_source(identifier.as_str(), target.as_str())
    }
    fn request_rerender(&mut self, view: RString) {
        self.host.request_rerender(view.as_str());
    }
}

/// Build a `PluginHost_TO_TO` trait object from a host reference, valid for one
/// call (the plugin never stores the host beyond the call).
fn host_bridge<'a>(host: &'a mut dyn PluginHost) -> PluginHost_TO_TO<'a, RBox<()>> {
    use abi_stable::sabi_trait::TD_Opaque;
    PluginHost_TO_TO::from_value(HostBridge { host }, TD_Opaque)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::manager::PluginManager;

    /// Locate a built example-plugin cdylib under the cargo target dir(s). Returns
    /// `None` if it hasn't been built (so the load test skips rather than fails in
    /// an environment where the example wasn't compiled).
    fn example_plugin_path(crate_name: &str) -> Option<std::path::PathBuf> {
        let ext = if cfg!(target_os = "windows") {
            "dll"
        } else if cfg!(target_os = "macos") {
            "dylib"
        } else {
            "so"
        };
        let lib_prefix = if cfg!(target_os = "windows") {
            ""
        } else {
            "lib"
        };
        let file = format!("{lib_prefix}{}.{ext}", crate_name.replace('-', "_"));

        // CARGO_MANIFEST_DIR is the repo root (the `reclass` package).
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let candidates = [
            root.join("target/debug").join(&file),
            root.join("target/release").join(&file),
        ];
        candidates.into_iter().find(|p| p.is_file())
    }

    #[test]
    fn loads_example_provider_so_and_reads_through_the_abi() {
        let Some(path) = example_plugin_path("example-provider") else {
            eprintln!("skipping: example-provider cdylib not built");
            return;
        };

        // Load + register through the SAME manager path as a built-in (the §1
        // parity table satisfied by a real `.so`).
        let mut mgr = PluginManager::new();
        let identifier = match mgr.load_native_plugin(&path) {
            Ok(id) => id,
            Err(e) => panic!("load failed: {e}"),
        };
        // "Example Provider" → "exampleprovider" (the derive_identifier rule).
        assert_eq!(identifier, "exampleprovider");

        // The provider is registered in the registry like a built-in.
        assert!(mgr.registry().find(&identifier).is_some());
        assert!(mgr.find_plugin(&identifier).is_some());

        // Attach: create the provider for a non-empty target and read through the
        // native vtable (the §8 hot path).
        let prov = match mgr.create_provider(&identifier, "demo") {
            Ok(p) => p,
            Err(e) => panic!("create_provider: {e}"),
        };
        assert_eq!(prov.size(), 256);
        assert_eq!(prov.name(), "ramp:demo");
        assert_eq!(prov.kind(), "Example");
        let mut buf = [0u8; 4];
        assert!(prov.read(2, &mut buf));
        // The ramp provider returns buf[i] == addr + i.
        assert_eq!(buf, [2, 3, 4, 5]);

        // An empty target is rejected with the plugin's surfaced error string.
        let err = match mgr.create_provider(&identifier, "") {
            Ok(_) => panic!("expected an error for an empty target"),
            Err(e) => e,
        };
        assert!(err.contains("empty target"), "got: {err}");
    }

    #[test]
    fn loads_example_ui_so_and_routes_a_command() {
        let Some(path) = example_plugin_path("example-ui") else {
            eprintln!("skipping: example-ui cdylib not built");
            return;
        };

        let mut mgr = PluginManager::new();
        let identifier = mgr.load_native_plugin(&path).expect("load example-ui");
        // "Example UI" → "exampleui". The UI-only plugin contributes no provider,
        // so it is NOT in the provider registry (and `find_plugin`, keyed on the
        // provider index, does not resolve it — by design).
        assert_eq!(identifier, "exampleui");
        assert!(mgr.registry().find(&identifier).is_none());

        // It contributes a Command + a Panel + a Dialog. The panel/dialog views
        // are indexed; `view_tree` pulls the panel's current tree across the ABI.
        let panel = mgr.view_tree("example.ui.panel").expect("panel tree");
        assert!(matches!(panel, ViewTree::Column(_)));
        assert!(mgr.view_tree("example.ui.dialog").is_some());

        // Route its palette command across the ABI; it shows a toast back through
        // the host bridge (proving the host callback surface crosses correctly).
        let mut host = crate::plugin::host::MockPluginHost::new();
        let res = mgr.handle_command("example.ui.ping", serde_json::Value::Null, &mut host);
        assert!(res.handled);
        assert_eq!(host.toasts(), ["Example UI: pong"]);

        // Drive the panel's button → it opens the dialog (host callback) and
        // re-renders the panel with an incremented open count (the Elm loop over
        // the ABI).
        let rerendered = mgr.handle_ui_event(
            "example.ui.panel",
            UiEvent::Clicked("example.ui.open".to_string()),
            &mut host,
        );
        assert!(rerendered.is_some());
        assert_eq!(host.opened_dialogs(), ["example.ui.dialog"]);
    }
}
