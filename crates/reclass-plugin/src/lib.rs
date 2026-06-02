//! `reclass-plugin` — the **author-facing SDK** for writing native Reclass plugins
//! (design §G "a published `reclass-plugin` SDK crate: the trait + `abi_stable`
//! glue + … generated export boilerplate"; §6 Phase 3).
//!
//! A plugin author depends on this crate, implements the plain-Rust [`Plugin`] and
//! (optionally) [`Provider`] traits using ordinary `String`/`Vec`, and calls
//! [`export_plugin!`] to generate the `abi_stable` root-module export. All the
//! conversion to the stable-ABI `Abi*`/`*_TO` types of `reclass-plugin-abi` lives
//! here, so plugin code never touches `RString`/`RVec` directly.
//!
//! The author types here intentionally mirror the host's
//! `reclass::plugin::{Plugin, Contribution, ViewTree, …}` shapes so a plugin reads
//! exactly like an in-tree one; the `into_abi` adapters bridge to the wire types.

#![allow(non_camel_case_types)]

/// Re-export `abi_stable` so [`export_plugin!`] can reference its
/// `prefix_type::PrefixTypeTrait` and `std_types` through a stable
/// `$crate::abi_stable::…` path (a plugin author need not depend on `abi_stable`
/// directly).
pub use abi_stable;
/// Re-export the `export_root_module` **attribute proc-macro** at the SDK root so
/// [`export_plugin!`] can apply it as `#[$crate::export_root_module]` (attribute
/// proc-macro paths must resolve to the macro directly — they don't follow a
/// module-qualified `$crate::abi_stable::…` indirection).
pub use abi_stable::export_root_module;
pub use reclass_plugin_abi as abi;

use abi_stable::{
    sabi_trait::TD_Opaque,
    std_types::{RBox, ROption, RSlice, RSliceMut, RString, RVec, Tuple2},
};
use reclass_plugin_abi::{
    AbiCommandResult, AbiCommandSlot, AbiContribution, AbiDialogResult, AbiDockSide, AbiManifest,
    AbiMemoryRegion, AbiProcessInfo, AbiProviderSpec, AbiTreeNode, AbiUiEvent, AbiViewTree,
    PluginHost_TO_TO, Plugin_TO, Provider_TO,
};

// ── re-export the wire trait-object aliases the macro/glue needs ──
pub use reclass_plugin_abi::{PluginMod, PluginModRef, Plugin_TO_TO, Provider_TO_TO};

// ═════════════════════════════════════════════════════════════════════════════
// Author-facing plain-Rust types (mirror the host contract).
// ═════════════════════════════════════════════════════════════════════════════

/// A declared capability (design §5). Tokens match the host's `Permission`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    ReadMemory,
    WriteMemory,
    Network,
    Filesystem,
    AddUi,
    AddProvider,
}

impl Permission {
    pub fn as_str(self) -> &'static str {
        match self {
            Permission::ReadMemory => "read_memory",
            Permission::WriteMemory => "write_memory",
            Permission::Network => "network",
            Permission::Filesystem => "filesystem",
            Permission::AddUi => "add_ui",
            Permission::AddProvider => "add_provider",
        }
    }
}

/// The plugin's static metadata (design §5). `kind` is always reported as
/// `"native"` across the ABI (this SDK only builds native cdylibs).
#[derive(Clone, Debug)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub permissions: Vec<Permission>,
}

impl Manifest {
    /// A manifest with empty author/description and the given permissions.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Manifest {
            name: name.into(),
            version: version.into(),
            author: String::new(),
            description: String::new(),
            permissions: Vec::new(),
        }
    }

    pub fn author(mut self, a: impl Into<String>) -> Self {
        self.author = a.into();
        self
    }
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = d.into();
        self
    }
    pub fn permissions(mut self, p: Vec<Permission>) -> Self {
        self.permissions = p;
        self
    }

    fn into_abi(&self) -> AbiManifest {
        AbiManifest {
            name: self.name.clone().into(),
            version: self.version.clone().into(),
            author: self.author.clone().into(),
            description: self.description.clone().into(),
            kind: RString::from("native"),
            permissions: self
                .permissions
                .iter()
                .map(|p| RString::from(p.as_str()))
                .collect(),
        }
    }
}

/// Where a command is surfaced (design §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandSlot {
    Menu,
    EditorContext,
    SourceMenu,
    Toolbar,
    Palette,
}

impl CommandSlot {
    fn into_abi(self) -> AbiCommandSlot {
        match self {
            CommandSlot::Menu => AbiCommandSlot::Menu,
            CommandSlot::EditorContext => AbiCommandSlot::EditorContext,
            CommandSlot::SourceMenu => AbiCommandSlot::SourceMenu,
            CommandSlot::Toolbar => AbiCommandSlot::Toolbar,
            CommandSlot::Palette => AbiCommandSlot::Palette,
        }
    }
}

/// Which edge a panel docks to (design §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockSide {
    Left,
    Right,
    Bottom,
}

impl DockSide {
    fn into_abi(self) -> AbiDockSide {
        match self {
            DockSide::Left => AbiDockSide::Left,
            DockSide::Right => AbiDockSide::Right,
            DockSide::Bottom => AbiDockSide::Bottom,
        }
    }
}

/// A node in a [`ViewTree::Tree`].
#[derive(Clone, Debug)]
pub struct TreeNode {
    pub id: String,
    pub label: String,
    pub children: Vec<TreeNode>,
}

impl TreeNode {
    pub fn leaf(id: impl Into<String>, label: impl Into<String>) -> Self {
        TreeNode {
            id: id.into(),
            label: label.into(),
            children: Vec::new(),
        }
    }

    fn into_abi(self) -> AbiTreeNode {
        AbiTreeNode {
            id: self.id.into(),
            label: self.label.into(),
            children: self.children.into_iter().map(TreeNode::into_abi).collect(),
        }
    }
}

/// The declarative widget tree a plugin contributes (design §3). Plain-Rust mirror
/// of the host's `ViewTree`; converted to [`AbiViewTree`] on the way out.
#[derive(Clone, Debug)]
pub enum ViewTree {
    Column(Vec<ViewTree>),
    Row(Vec<ViewTree>),
    Group {
        title: String,
        child: Box<ViewTree>,
    },
    Label(String),
    Button {
        id: String,
        label: String,
    },
    TextInput {
        id: String,
        value: String,
        placeholder: String,
    },
    Checkbox {
        id: String,
        label: String,
        checked: bool,
    },
    Dropdown {
        id: String,
        options: Vec<String>,
        selected: usize,
    },
    Table {
        id: String,
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Tree {
        id: String,
        nodes: Vec<TreeNode>,
    },
    KeyValue(Vec<(String, String)>),
    Separator,
}

impl ViewTree {
    pub fn group(title: impl Into<String>, child: ViewTree) -> ViewTree {
        ViewTree::Group {
            title: title.into(),
            child: Box::new(child),
        }
    }

    /// Convert to the stable-ABI [`AbiViewTree`] (the only adjustment is
    /// `usize` → `u64` for `selected`, since `usize` is not a fixed-layout type).
    pub fn into_abi(self) -> AbiViewTree {
        match self {
            ViewTree::Column(cs) => {
                AbiViewTree::Column(cs.into_iter().map(ViewTree::into_abi).collect())
            }
            ViewTree::Row(cs) => AbiViewTree::Row(cs.into_iter().map(ViewTree::into_abi).collect()),
            ViewTree::Group { title, child } => AbiViewTree::Group {
                title: title.into(),
                child: RBox::new(child.into_abi()),
            },
            ViewTree::Label(s) => AbiViewTree::Label(s.into()),
            ViewTree::Button { id, label } => AbiViewTree::Button {
                id: id.into(),
                label: label.into(),
            },
            ViewTree::TextInput {
                id,
                value,
                placeholder,
            } => AbiViewTree::TextInput {
                id: id.into(),
                value: value.into(),
                placeholder: placeholder.into(),
            },
            ViewTree::Checkbox { id, label, checked } => AbiViewTree::Checkbox {
                id: id.into(),
                label: label.into(),
                checked,
            },
            ViewTree::Dropdown {
                id,
                options,
                selected,
            } => AbiViewTree::Dropdown {
                id: id.into(),
                options: options.into_iter().map(RString::from).collect(),
                selected: selected as u64,
            },
            ViewTree::Table { id, columns, rows } => AbiViewTree::Table {
                id: id.into(),
                columns: columns.into_iter().map(RString::from).collect(),
                rows: rows
                    .into_iter()
                    .map(|r| r.into_iter().map(RString::from).collect::<RVec<_>>())
                    .collect(),
            },
            ViewTree::Tree { id, nodes } => AbiViewTree::Tree {
                id: id.into(),
                nodes: nodes.into_iter().map(TreeNode::into_abi).collect(),
            },
            ViewTree::KeyValue(kvs) => AbiViewTree::KeyValue(
                kvs.into_iter()
                    .map(|(k, v)| Tuple2(k.into(), v.into()))
                    .collect(),
            ),
            ViewTree::Separator => AbiViewTree::Separator,
        }
    }
}

/// An event the host routes back to the plugin (design §3).
#[derive(Clone, Debug)]
pub enum UiEvent {
    Clicked(String),
    Submitted { id: String, text: String },
    Toggled { id: String, on: bool },
    RowSelected { table: String, row: usize },
}

impl UiEvent {
    fn from_abi(ev: AbiUiEvent) -> UiEvent {
        match ev {
            AbiUiEvent::Clicked(s) => UiEvent::Clicked(s.into()),
            AbiUiEvent::Submitted { id, text } => UiEvent::Submitted {
                id: id.into(),
                text: text.into(),
            },
            AbiUiEvent::Toggled { id, on } => UiEvent::Toggled { id: id.into(), on },
            AbiUiEvent::RowSelected { table, row } => UiEvent::RowSelected {
                table: table.into(),
                row: row as usize,
            },
        }
    }
}

/// The outcome a dialog reports back (design §3).
#[derive(Clone, Debug)]
pub enum DialogResult {
    Submitted { values: Vec<(String, String)> },
    Cancelled,
}

impl DialogResult {
    fn from_abi(r: AbiDialogResult) -> DialogResult {
        match r {
            AbiDialogResult::Submitted { values } => DialogResult::Submitted {
                values: values
                    .into_iter()
                    .map(|Tuple2(k, v)| (k.into(), v.into()))
                    .collect(),
            },
            AbiDialogResult::Cancelled => DialogResult::Cancelled,
        }
    }
}

/// What a plugin contributes (design §2).
#[derive(Clone, Debug)]
pub enum Contribution {
    Provider {
        provides_process_list: bool,
    },
    Command {
        id: String,
        title: String,
        slot: CommandSlot,
    },
    Panel {
        id: String,
        title: String,
        dock: DockSide,
        initial: ViewTree,
    },
    Dialog {
        id: String,
        title: String,
        initial: ViewTree,
    },
    StatusItem {
        id: String,
        initial: ViewTree,
    },
}

impl Contribution {
    fn into_abi(self) -> AbiContribution {
        match self {
            Contribution::Provider {
                provides_process_list,
            } => AbiContribution::Provider(AbiProviderSpec {
                provides_process_list,
            }),
            Contribution::Command { id, title, slot } => AbiContribution::Command {
                id: id.into(),
                title: title.into(),
                slot: slot.into_abi(),
            },
            Contribution::Panel {
                id,
                title,
                dock,
                initial,
            } => AbiContribution::Panel {
                id: id.into(),
                title: title.into(),
                dock: dock.into_abi(),
                initial: initial.into_abi(),
            },
            Contribution::Dialog { id, title, initial } => AbiContribution::Dialog {
                id: id.into(),
                title: title.into(),
                initial: initial.into_abi(),
            },
            Contribution::StatusItem { id, initial } => AbiContribution::StatusItem {
                id: id.into(),
                initial: initial.into_abi(),
            },
        }
    }
}

/// The outcome of [`Plugin::handle_command`] (design §2).
#[derive(Clone, Debug, Default)]
pub struct CommandResult {
    pub handled: bool,
    pub toast: Option<String>,
}

impl CommandResult {
    pub fn handled() -> Self {
        CommandResult {
            handled: true,
            toast: None,
        }
    }
    pub fn toast(msg: impl Into<String>) -> Self {
        CommandResult {
            handled: true,
            toast: Some(msg.into()),
        }
    }
    fn into_abi(self) -> AbiCommandResult {
        AbiCommandResult {
            handled: self.handled,
            toast: self.toast.map(RString::from).into(),
        }
    }
}

/// A process the host can attach to (design §2).
#[derive(Clone, Debug, Default)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub path: String,
    pub is_32bit: bool,
}

impl ProcessInfo {
    fn into_abi(self) -> AbiProcessInfo {
        AbiProcessInfo {
            pid: self.pid,
            name: self.name.into(),
            path: self.path.into(),
            is_32bit: self.is_32bit,
        }
    }
}

/// A memory region a provider exposes.
#[derive(Clone, Debug, Default)]
pub struct MemoryRegion {
    pub base: u64,
    pub size: u64,
    pub readable: bool,
    pub writable: bool,
    pub executable: bool,
    pub module_name: String,
    /// 0 = Image, 1 = Mapped, 2 = Private.
    pub region_type: u8,
}

// ═════════════════════════════════════════════════════════════════════════════
// The author-facing traits.
// ═════════════════════════════════════════════════════════════════════════════

/// The host callback surface, plain-Rust, handed to the plugin. Wraps the
/// `PluginHost_TO_TO` trait object so the plugin author never touches `RString`.
pub struct Host<'a> {
    inner: &'a mut PluginHost_TO_TO<'a, RBox<()>>,
}

impl<'a> Host<'a> {
    fn new(inner: &'a mut PluginHost_TO_TO<'a, RBox<()>>) -> Self {
        Host { inner }
    }

    pub fn read_provider(&self, addr: u64, buf: &mut [u8]) -> bool {
        self.inner
            .read_provider(addr, RSliceMut::from_mut_slice(buf))
    }
    pub fn provider_name(&self) -> String {
        self.inner.provider_name().into()
    }
    pub fn show_toast(&mut self, msg: impl Into<String>) {
        self.inner.show_toast(msg.into().into());
    }
    pub fn open_dialog(&mut self, id: impl Into<String>) {
        self.inner.open_dialog(id.into().into());
    }
    pub fn close_dialog(&mut self, id: impl Into<String>) {
        self.inner.close_dialog(id.into().into());
    }
    pub fn get_setting(&self, key: &str) -> Option<String> {
        self.inner
            .get_setting(RString::from(key))
            .into_option()
            .map(Into::into)
    }
    pub fn set_setting(&mut self, key: &str, val: &str) {
        self.inner
            .set_setting(RString::from(key), RString::from(val));
    }
    pub fn add_node(&mut self, parent_path: &str, node_kind: &str) -> bool {
        self.inner
            .add_node(RString::from(parent_path), RString::from(node_kind))
    }
    pub fn set_data_source(&mut self, identifier: &str, target: &str) -> bool {
        self.inner
            .set_data_source(RString::from(identifier), RString::from(target))
    }
    pub fn request_rerender(&mut self, view: &str) {
        self.inner.request_rerender(RString::from(view));
    }
}

/// The memory-reading surface a provider plugin implements (plain-Rust mirror of
/// `reclass::provider::Provider`). Only [`read`](Provider::read) +
/// [`size`](Provider::size) are required.
pub trait Provider: Send + Sync + 'static {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool;
    fn size(&self) -> i32;
    fn name(&self) -> String {
        String::new()
    }
    fn kind(&self) -> String {
        "File".to_string()
    }
    fn base(&self) -> u64 {
        0
    }
    fn pointer_size(&self) -> i32 {
        8
    }
    fn is_live(&self) -> bool {
        false
    }
    fn is_writable(&self) -> bool {
        false
    }
    fn write(&mut self, _addr: u64, _data: &[u8]) -> bool {
        false
    }
    fn enumerate_regions(&self) -> Vec<MemoryRegion> {
        Vec::new()
    }
}

/// The host-side plugin trait, plain-Rust (design §2). Identical in spirit to the
/// in-tree `reclass::plugin::Plugin`.
pub trait Plugin: Send + Sync + 'static {
    fn manifest(&self) -> Manifest;
    fn contributions(&self) -> Vec<Contribution>;
    fn activate(&mut self, _host: &mut Host<'_>) {}
    fn deactivate(&mut self) {}
    fn handle_command(
        &mut self,
        _id: &str,
        _args_json: &str,
        _host: &mut Host<'_>,
    ) -> CommandResult {
        CommandResult::default()
    }
    fn handle_ui_event(
        &mut self,
        _view: &str,
        _ev: UiEvent,
        _host: &mut Host<'_>,
    ) -> Option<ViewTree> {
        None
    }
    fn handle_dialog_closed(
        &mut self,
        _view: &str,
        _result: DialogResult,
        _host: &mut Host<'_>,
    ) -> CommandResult {
        CommandResult::default()
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Adapters: plain-Rust author traits → the abi_stable trait objects. These are the
// glue the `export_plugin!` macro stitches into the root module. Every adapter
// boundary is panic-guarded with `catch_unwind` so a plugin panic can never unwind
// across the FFI boundary (which is UB — design §7.B / §8).
// ═════════════════════════════════════════════════════════════════════════════

/// Wraps an author [`Provider`] as the stable-ABI [`Provider_TO`].
struct ProviderAdapter<P: Provider>(P);

impl<P: Provider> Provider_TO for ProviderAdapter<P> {
    fn read(&self, addr: u64, mut buf: RSliceMut<'_, u8>) -> bool {
        let p = &self.0;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            p.read(addr, buf.as_mut_slice())
        }))
        .unwrap_or(false)
    }
    fn size(&self) -> i32 {
        let p = &self.0;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.size())).unwrap_or(0)
    }
    fn name(&self) -> RString {
        let p = &self.0;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.name()))
            .unwrap_or_default()
            .into()
    }
    fn kind(&self) -> RString {
        let p = &self.0;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.kind()))
            .unwrap_or_else(|_| "File".to_string())
            .into()
    }
    fn base(&self) -> u64 {
        let p = &self.0;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.base())).unwrap_or(0)
    }
    fn pointer_size(&self) -> i32 {
        let p = &self.0;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.pointer_size())).unwrap_or(8)
    }
    fn is_live(&self) -> bool {
        let p = &self.0;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.is_live())).unwrap_or(false)
    }
    fn is_writable(&self) -> bool {
        let p = &self.0;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.is_writable())).unwrap_or(false)
    }
    fn write(&mut self, addr: u64, data: RSlice<'_, u8>) -> bool {
        let p = &mut self.0;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            p.write(addr, data.as_slice())
        }))
        .unwrap_or(false)
    }
    fn enumerate_regions(&self) -> RVec<AbiMemoryRegion> {
        let p = &self.0;
        let regions =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.enumerate_regions()))
                .unwrap_or_default();
        regions
            .into_iter()
            .map(|r| AbiMemoryRegion {
                base: r.base,
                size: r.size,
                readable: r.readable,
                writable: r.writable,
                executable: r.executable,
                module_name: r.module_name.into(),
                region_type: r.region_type,
            })
            .collect()
    }
}

/// Build a `Provider_TO_TO` trait object from an author [`Provider`] (used by the
/// `create_provider` export the macro generates).
pub fn export_provider<P: Provider>(p: P) -> Provider_TO_TO<'static, RBox<()>> {
    Provider_TO_TO::from_value(ProviderAdapter(p), TD_Opaque)
}

/// A placeholder [`Provider`] for **UI-only** plugins that contribute no data
/// source: it satisfies `export_plugin!`'s `create_provider` arm's type
/// requirement while always reporting "no provider". Use it as the error-only
/// return type:
/// ```ignore
/// create_provider: |_| -> Result<reclass_plugin::NoProvider, String> {
///     Err("this plugin contributes no provider".into())
/// },
/// ```
pub enum NoProvider {}

impl Provider for NoProvider {
    fn read(&self, _addr: u64, _buf: &mut [u8]) -> bool {
        match *self {}
    }
    fn size(&self) -> i32 {
        match *self {}
    }
}

/// Wraps an author [`Plugin`] as the stable-ABI [`Plugin_TO`].
struct PluginAdapter<P: Plugin>(P);

impl<P: Plugin> Plugin_TO for PluginAdapter<P> {
    fn manifest(&self) -> AbiManifest {
        let p = &self.0;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.manifest().into_abi()))
            .unwrap_or_else(|_| Manifest::new("<panicked>", "0").into_abi())
    }
    fn contributions(&self) -> RVec<AbiContribution> {
        let p = &self.0;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            p.contributions()
                .into_iter()
                .map(Contribution::into_abi)
                .collect::<RVec<_>>()
        }))
        .unwrap_or_default()
    }
    fn activate(&mut self, host: &mut PluginHost_TO_TO<'_, RBox<()>>) {
        let p = &mut self.0;
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // SAFETY: shorten the host borrow to the call; the Host wrapper does
            // not outlive this method.
            let mut h = Host::new(unsafe { std::mem::transmute(host) });
            p.activate(&mut h);
        }));
    }
    fn deactivate(&mut self) {
        let p = &mut self.0;
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.deactivate()));
    }
    fn handle_command(
        &mut self,
        id: RString,
        args_json: RString,
        host: &mut PluginHost_TO_TO<'_, RBox<()>>,
    ) -> AbiCommandResult {
        let p = &mut self.0;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut h = Host::new(unsafe { std::mem::transmute(host) });
            p.handle_command(id.as_str(), args_json.as_str(), &mut h)
                .into_abi()
        }))
        .unwrap_or_default()
    }
    fn handle_ui_event(
        &mut self,
        view: RString,
        ev: AbiUiEvent,
        host: &mut PluginHost_TO_TO<'_, RBox<()>>,
    ) -> ROption<AbiViewTree> {
        let p = &mut self.0;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut h = Host::new(unsafe { std::mem::transmute(host) });
            p.handle_ui_event(view.as_str(), UiEvent::from_abi(ev), &mut h)
                .map(ViewTree::into_abi)
                .into()
        }))
        .unwrap_or(ROption::RNone)
    }
    fn handle_dialog_closed(
        &mut self,
        view: RString,
        result: AbiDialogResult,
        host: &mut PluginHost_TO_TO<'_, RBox<()>>,
    ) -> AbiCommandResult {
        let p = &mut self.0;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut h = Host::new(unsafe { std::mem::transmute(host) });
            p.handle_dialog_closed(view.as_str(), DialogResult::from_abi(result), &mut h)
                .into_abi()
        }))
        .unwrap_or_default()
    }
}

/// Build a `Plugin_TO_TO` trait object from an author [`Plugin`] (used by the
/// `new_plugin` export the macro generates).
pub fn export_plugin_value<P: Plugin>(p: P) -> Plugin_TO_TO<'static, RBox<()>> {
    Plugin_TO_TO::from_value(PluginAdapter(p), TD_Opaque)
}

/// Build the ABI list of processes from author [`ProcessInfo`]s (used by the
/// `enumerate_processes` export).
pub fn export_processes(procs: Vec<ProcessInfo>) -> RVec<AbiProcessInfo> {
    procs.into_iter().map(ProcessInfo::into_abi).collect()
}

/// Build an `RResult` for the `create_provider` export from a `Result`.
pub fn provider_result<P: Provider>(
    r: Result<P, String>,
) -> abi_stable::std_types::RResult<Provider_TO_TO<'static, RBox<()>>, RString> {
    match r {
        Ok(p) => abi_stable::std_types::RResult::ROk(export_provider(p)),
        Err(e) => abi_stable::std_types::RResult::RErr(e.into()),
    }
}

/// Generate the `abi_stable` root-module export for a plugin (design §6 Phase 3 —
/// "root-module export, version/layout check on load"; replaces the C++
/// `CreatePlugin()`).
///
/// Usage in a plugin's `lib.rs`:
/// ```ignore
/// reclass_plugin::export_plugin! {
///     new_plugin: || MyPlugin::default(),
///     // optional provider factory (omit for UI-only plugins):
///     can_handle: |target: &str| !target.is_empty(),
///     create_provider: |target: &str| -> Result<MyProvider, String> { Ok(MyProvider::open(target)?) },
///     enumerate_processes: || Vec::new(),
/// }
/// ```
#[macro_export]
macro_rules! export_plugin {
    (
        new_plugin: $new_plugin:expr,
        can_handle: $can_handle:expr,
        create_provider: $create_provider:expr,
        enumerate_processes: $enumerate_processes:expr $(,)?
    ) => {
        #[no_mangle]
        extern "C" fn __reclass_new_plugin(
        ) -> $crate::Plugin_TO_TO<'static, $crate::abi_stable::std_types::RBox<()>> {
            let f = $new_plugin;
            $crate::export_plugin_value(f())
        }

        #[no_mangle]
        extern "C" fn __reclass_can_handle(target: $crate::abi_stable::std_types::RString) -> bool {
            let f = $can_handle;
            ::std::panic::catch_unwind(|| f(target.as_str())).unwrap_or(false)
        }

        #[no_mangle]
        extern "C" fn __reclass_create_provider(
            target: $crate::abi_stable::std_types::RString,
        ) -> $crate::abi_stable::std_types::RResult<
            $crate::Provider_TO_TO<'static, $crate::abi_stable::std_types::RBox<()>>,
            $crate::abi_stable::std_types::RString,
        > {
            let f = $create_provider;
            match ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| f(target.as_str())))
            {
                ::std::result::Result::Ok(r) => $crate::provider_result(r),
                ::std::result::Result::Err(_) => $crate::abi_stable::std_types::RResult::RErr(
                    $crate::abi_stable::std_types::RString::from("plugin create_provider panicked"),
                ),
            }
        }

        #[no_mangle]
        extern "C" fn __reclass_enumerate_processes(
        ) -> $crate::abi_stable::std_types::RVec<$crate::abi::AbiProcessInfo> {
            let f = $enumerate_processes;
            match ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| f())) {
                ::std::result::Result::Ok(procs) => $crate::export_processes(procs),
                ::std::result::Result::Err(_) => $crate::abi_stable::std_types::RVec::new(),
            }
        }

        #[$crate::export_root_module]
        pub fn __reclass_instantiate_root_module() -> $crate::PluginModRef {
            use $crate::abi_stable::prefix_type::PrefixTypeTrait;
            $crate::PluginMod {
                new_plugin: __reclass_new_plugin,
                create_provider: __reclass_create_provider,
                can_handle: __reclass_can_handle,
                enumerate_processes: __reclass_enumerate_processes,
            }
            .leak_into_prefix()
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DemoProvider {
        bytes: Vec<u8>,
    }
    impl Provider for DemoProvider {
        fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            let start = addr as usize;
            let end = match start.checked_add(buf.len()) {
                Some(e) => e,
                None => return false,
            };
            if end > self.bytes.len() {
                return false;
            }
            buf.copy_from_slice(&self.bytes[start..end]);
            true
        }
        fn size(&self) -> i32 {
            self.bytes.len() as i32
        }
        fn name(&self) -> String {
            "demo".to_string()
        }
    }

    #[test]
    fn provider_adapter_reads_through_the_abi_trait() {
        let to = export_provider(DemoProvider {
            bytes: vec![1, 2, 3, 4],
        });
        assert_eq!(to.size(), 4);
        assert_eq!(to.name().as_str(), "demo");
        let mut buf = [0u8; 2];
        assert!(to.read(1, RSliceMut::from_mut_slice(&mut buf)));
        assert_eq!(buf, [2, 3]);
        // Out of range fails, no panic.
        assert!(!to.read(3, RSliceMut::from_mut_slice(&mut [0u8; 2])));
    }

    #[test]
    fn view_tree_into_abi_is_mechanical() {
        let t = ViewTree::Dropdown {
            id: "m".into(),
            options: vec!["a".into(), "b".into()],
            selected: 1,
        };
        match t.into_abi() {
            AbiViewTree::Dropdown { selected, .. } => assert_eq!(selected, 1u64),
            _ => panic!("expected dropdown"),
        }
    }

    #[test]
    fn provider_result_maps_ok_and_err() {
        let ok = provider_result::<DemoProvider>(Ok(DemoProvider { bytes: vec![9] }));
        assert!(matches!(ok, abi_stable::std_types::RResult::ROk(_)));
        let err = provider_result::<DemoProvider>(Err("nope".to_string()));
        match err {
            abi_stable::std_types::RResult::RErr(e) => assert_eq!(e.as_str(), "nope"),
            _ => panic!("expected err"),
        }
    }
}
