//! `reclass-plugin-abi` — the **stable-ABI contract** shared by the Reclass host
//! (when built with the `plugins` feature) and every native plugin cdylib.
//!
//! Design anchors:
//! - §2/§3 — the `Plugin`/`PluginHost`/`Contribution`/`ViewTree` contract, here
//!   mirrored with `abi_stable` types (`RString`/`RVec`/`RBox`/…) so it crosses
//!   the native FFI boundary with a documented, version-checked layout.
//! - §8 — "`Provider` across the ABI = `RBox<dyn Provider_TO>`": the provider is a
//!   `#[sabi_trait]` object, so a plugin `read()` is a direct native vtable call on
//!   the hot path (no per-call marshalling).
//! - §1 (parity table) — the `extern "C"` root-module export `abi_stable` generates
//!   is the single C symbol, replacing the C++ `CreatePlugin()`.
//!
//! Every `Abi*` enum/struct here has a variant **shape identical** to the
//! plain-Rust type in `reclass`'s `src/plugin/{view,contract}.rs`, so the
//! host-side conversion is mechanical (the only adjustment is `usize` → `u64` for
//! indices/rows, since `usize` is not a fixed-layout ABI type).
//!
//! This crate is an rlib linked *into* each cdylib (and into the host behind
//! `plugins`); it is never itself a cdylib.

#![allow(non_camel_case_types)]
// `*_TO` sabi-trait object names are abi_stable convention.
// `#[sabi_trait]` (abi_stable 0.11) emits trait `impl`s inside an anonymous const,
// which newer rustc flags as `non_local_definitions`. This is macro-generated code
// we don't control; silence it here so the crate builds warning-clean.
#![allow(unknown_lints)]
#![allow(non_local_definitions)]

use abi_stable::{
    declare_root_module_statics,
    library::RootModule,
    package_version_strings, sabi_trait,
    sabi_types::VersionStrings,
    std_types::{RBox, ROption, RSlice, RSliceMut, RString, RVec, Tuple2},
    StableAbi,
};

// ─────────────────────────────────────────────────────────────────────────────
// (a) Manifest — mirrors reclass::plugin::PluginManifest's wire-relevant fields.
//     `kind`/`permissions` are carried as strings to keep the ABI a flat,
//     stable contract (the host maps them back to its PluginKind/Permission).
// ─────────────────────────────────────────────────────────────────────────────

/// The plugin's static metadata across the ABI (design §5). `kind` is the
/// `plugin.toml` token ("native" for a real cdylib) and `permissions` are the
/// `Permission::as_str()` tokens; the host maps both back to its own enums.
#[repr(C)]
#[derive(StableAbi, Clone, Debug)]
pub struct AbiManifest {
    pub name: RString,
    pub version: RString,
    pub author: RString,
    pub description: RString,
    pub kind: RString,
    pub permissions: RVec<RString>,
}

// ─────────────────────────────────────────────────────────────────────────────
// (b) ViewTree — the declarative UI vocabulary (design §3). Variant shape is
//     identical to src/plugin/view.rs::ViewTree (usize→u64 for `selected`/`row`).
// ─────────────────────────────────────────────────────────────────────────────

/// A node in an [`AbiViewTree::Tree`] (mirror of `view::TreeNode`).
#[repr(C)]
#[derive(StableAbi, Clone, Debug)]
pub struct AbiTreeNode {
    pub id: RString,
    pub label: RString,
    pub children: RVec<AbiTreeNode>,
}

/// The declarative widget tree a plugin contributes; the host renders it
/// (design §3). Mirror of `reclass::plugin::ViewTree`.
#[repr(C)]
#[derive(StableAbi, Clone, Debug)]
pub enum AbiViewTree {
    Column(RVec<AbiViewTree>),
    Row(RVec<AbiViewTree>),
    Group {
        title: RString,
        child: RBox<AbiViewTree>,
    },
    Label(RString),
    Button {
        id: RString,
        label: RString,
    },
    TextInput {
        id: RString,
        value: RString,
        placeholder: RString,
    },
    Checkbox {
        id: RString,
        label: RString,
        checked: bool,
    },
    Dropdown {
        id: RString,
        options: RVec<RString>,
        selected: u64,
    },
    Table {
        id: RString,
        columns: RVec<RString>,
        rows: RVec<RVec<RString>>,
    },
    Tree {
        id: RString,
        nodes: RVec<AbiTreeNode>,
    },
    KeyValue(RVec<Tuple2<RString, RString>>),
    Separator,
}

// ─────────────────────────────────────────────────────────────────────────────
// (c) UiEvent — mirror of src/plugin/view.rs::UiEvent (usize→u64 for `row`).
// ─────────────────────────────────────────────────────────────────────────────

/// An event the host routes back to the plugin (design §3). Mirror of
/// `reclass::plugin::UiEvent`.
#[repr(C)]
#[derive(StableAbi, Clone, Debug)]
pub enum AbiUiEvent {
    Clicked(RString),
    Submitted { id: RString, text: RString },
    Toggled { id: RString, on: bool },
    RowSelected { table: RString, row: u64 },
}

// ─────────────────────────────────────────────────────────────────────────────
// (d) Contribution + the small supporting enums/structs.
// ─────────────────────────────────────────────────────────────────────────────

/// Where a command is surfaced (design §3). Mirror of
/// `reclass::plugin::CommandSlot`.
#[repr(C)]
#[derive(StableAbi, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbiCommandSlot {
    Menu,
    EditorContext,
    SourceMenu,
    Toolbar,
    Palette,
}

/// Which edge a panel docks to (design §3). Mirror of `reclass::plugin::DockSide`.
#[repr(C)]
#[derive(StableAbi, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbiDockSide {
    Left,
    Right,
    Bottom,
}

/// A process the host can attach to (design §2). Mirror of
/// `reclass::plugin::ProcessInfo`.
#[repr(C)]
#[derive(StableAbi, Clone, Debug)]
pub struct AbiProcessInfo {
    pub pid: u32,
    pub name: RString,
    pub path: RString,
    pub is_32bit: bool,
}

/// A memory region a provider exposes (mirror of `reclass::provider::MemoryRegion`
/// — `region_type` carried as a u8 matching the `RegionType` discriminant, and
/// the flags packed individually to stay a flat ABI struct).
#[repr(C)]
#[derive(StableAbi, Clone, Debug)]
pub struct AbiMemoryRegion {
    pub base: u64,
    pub size: u64,
    pub readable: bool,
    pub writable: bool,
    pub executable: bool,
    pub module_name: RString,
    /// 0 = Image, 1 = Mapped, 2 = Private (the `RegionType` discriminant).
    pub region_type: u8,
}

/// The spec a [`AbiContribution::Provider`] carries. The provider itself is built
/// lazily via [`PluginMod::create_provider`] keyed on the derived identifier +
/// target, so the spec only needs to advertise the can-handle / enumerate surface
/// (the factory lives in the root module so it returns a fresh `Provider_TO`).
#[repr(C)]
#[derive(StableAbi, Clone, Debug)]
pub struct AbiProviderSpec {
    /// Whether this provider advertises a process list (the C++
    /// `providesProcessList()`). Process enumeration itself is requested through
    /// the root module (`enumerate_processes`).
    pub provides_process_list: bool,
}

/// The outcome of [`Plugin_TO::handle_command`] (design §2). Mirror of
/// `reclass::plugin::CommandResult`.
#[repr(C)]
#[derive(StableAbi, Clone, Debug)]
pub struct AbiCommandResult {
    pub handled: bool,
    pub toast: ROption<RString>,
}

impl Default for AbiCommandResult {
    fn default() -> Self {
        AbiCommandResult {
            handled: false,
            toast: ROption::RNone,
        }
    }
}

/// The outcome a contributed dialog reports back (design §3 — the generalized C++
/// `selectTarget`). Mirror of `reclass::plugin::DialogResult`.
#[repr(C)]
#[derive(StableAbi, Clone, Debug)]
pub enum AbiDialogResult {
    Submitted {
        values: RVec<Tuple2<RString, RString>>,
    },
    Cancelled,
}

/// What a plugin contributes (design §2). Mirror of
/// `reclass::plugin::Contribution`.
#[repr(C)]
#[derive(StableAbi, Clone, Debug)]
pub enum AbiContribution {
    Provider(AbiProviderSpec),
    Command {
        id: RString,
        title: RString,
        slot: AbiCommandSlot,
    },
    Panel {
        id: RString,
        title: RString,
        dock: AbiDockSide,
        initial: AbiViewTree,
    },
    Dialog {
        id: RString,
        title: RString,
        initial: AbiViewTree,
    },
    StatusItem {
        id: RString,
        initial: AbiViewTree,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// (e) Provider_TO — the §8 "Provider across the ABI = RBox<dyn Provider_TO>".
//     Mirrors crate::provider::Provider's required + the relevant optional
//     overrides, so a plugin read() is a direct native vtable call.
// ─────────────────────────────────────────────────────────────────────────────

/// The provider trait object that crosses the ABI (design §8). A plugin returns
/// `RBox<Provider_TO>`; the host wraps it in a `reclass::provider::Provider`
/// adapter, so `read()` is a direct native call on the refresh hot path.
///
/// Required: [`read`](Provider_TO::read) + [`size`](Provider_TO::size). The rest
/// mirror `Provider`'s optional virtual defaults. The last method is tagged
/// `#[sabi(last_prefix_field)]` so the trait can grow new methods at the end in a
/// later ABI version without breaking older plugins (abi_stable prefix-type rule).
#[sabi_trait]
pub trait Provider_TO: Send + Sync {
    /// `read(addr, buf)` → success (the hot path).
    fn read(&self, addr: u64, buf: RSliceMut<'_, u8>) -> bool;

    /// `size()` — logical size; 0 = invalid.
    fn size(&self) -> i32;

    fn name(&self) -> RString {
        RString::new()
    }

    fn kind(&self) -> RString {
        RString::from("File")
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

    fn write(&mut self, _addr: u64, _data: RSlice<'_, u8>) -> bool {
        false
    }

    #[sabi(last_prefix_field)]
    fn enumerate_regions(&self) -> RVec<AbiMemoryRegion> {
        RVec::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// (f) PluginHost_TO — the host callback surface a plugin talks back through
//     (design §2). Mirror of src/plugin/host.rs::PluginHost.
// ─────────────────────────────────────────────────────────────────────────────

/// The controlled host callback surface across the ABI (design §2). The host
/// implements this; the plugin receives an `&mut PluginHost_TO` in `activate` /
/// `handle_*`.
#[sabi_trait]
pub trait PluginHost_TO {
    /// Read bytes from the active provider into `buf`; returns success.
    fn read_provider(&self, addr: u64, buf: RSliceMut<'_, u8>) -> bool;

    /// The active provider's display name.
    fn provider_name(&self) -> RString;

    /// Show a short transient message.
    fn show_toast(&mut self, msg: RString);

    /// Open a contributed dialog by id (the generalized C++ `selectTarget`).
    fn open_dialog(&mut self, id: RString);

    /// Close a contributed dialog by id.
    fn close_dialog(&mut self, id: RString);

    /// Read a persisted per-plugin setting.
    fn get_setting(&self, key: RString) -> ROption<RString>;

    /// Persist a per-plugin setting.
    fn set_setting(&mut self, key: RString, val: RString);

    /// Add a node to the active document tree under `parent_path`.
    fn add_node(&mut self, parent_path: RString, node_kind: RString) -> bool;

    /// Switch the active document's data source to `identifier` for `target`.
    fn set_data_source(&mut self, identifier: RString, target: RString) -> bool;

    /// Ask the host to re-render the contributed view `view`.
    #[sabi(last_prefix_field)]
    fn request_rerender(&mut self, view: RString);
}

// ─────────────────────────────────────────────────────────────────────────────
// (g) Plugin_TO — the plugin trait object (design §2). Mirror of
//     src/plugin/contract.rs::Plugin.
// ─────────────────────────────────────────────────────────────────────────────

/// The plugin trait object across the ABI (design §2). The host loads a cdylib,
/// calls [`PluginMod::new_plugin`] to get one, then adapts it into a
/// `Box<dyn reclass::plugin::Plugin>` so the EXISTING `PluginManager`
/// registration flow is reused unchanged (the §1 parity table satisfied by a real
/// `.so`).
#[sabi_trait]
pub trait Plugin_TO: Send + Sync {
    /// The plugin's static metadata.
    fn manifest(&self) -> AbiManifest;

    /// What this plugin contributes (read once at registration).
    fn contributions(&self) -> RVec<AbiContribution>;

    /// Lifecycle: activated.
    fn activate(&mut self, _host: &mut PluginHost_TO_TO<'_, RBox<()>>) {}

    /// Lifecycle: about to be disabled/unloaded.
    fn deactivate(&mut self) {}

    /// Handle a `Command` contribution being invoked. `args_json` is opaque JSON.
    fn handle_command(
        &mut self,
        _id: RString,
        _args_json: RString,
        _host: &mut PluginHost_TO_TO<'_, RBox<()>>,
    ) -> AbiCommandResult {
        AbiCommandResult::default()
    }

    /// Handle a UI event from a contributed view; `Some(tree)` re-renders it.
    fn handle_ui_event(
        &mut self,
        _view: RString,
        _ev: AbiUiEvent,
        _host: &mut PluginHost_TO_TO<'_, RBox<()>>,
    ) -> ROption<AbiViewTree> {
        ROption::RNone
    }

    /// A contributed dialog closed — report its outcome back to the plugin.
    #[sabi(last_prefix_field)]
    fn handle_dialog_closed(
        &mut self,
        _view: RString,
        _result: AbiDialogResult,
        _host: &mut PluginHost_TO_TO<'_, RBox<()>>,
    ) -> AbiCommandResult {
        AbiCommandResult::default()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The root module — the `abi_stable` equivalent of the C++ `CreatePlugin()`
// export. The generated `extern "C"` bootstrap symbol is the single C symbol
// (design §1 parity table). The host loads it with a built-in version + layout
// check that refuses a mismatched plugin (design §7.B [+], strictly safer than
// C++'s raw dlopen).
// ─────────────────────────────────────────────────────────────────────────────

/// The plugin's root module — its entry points. Built with
/// `#[sabi(kind(Prefix(..)))]` so new fields can be appended in a later ABI
/// version without breaking older plugins (the prefix-type rule).
#[repr(C)]
#[derive(StableAbi)]
#[sabi(kind(Prefix(prefix_ref = PluginModRef)))]
#[sabi(missing_field(panic))]
pub struct PluginMod {
    /// Construct the plugin instance.
    pub new_plugin: extern "C" fn() -> Plugin_TO_TO<'static, RBox<()>>,

    /// Create a provider for `(identifier, target)`. Returns
    /// `RErr(message)` on failure (the C++ `createProvider` error string,
    /// design §7.A [fix]). Provider crosses as `Provider_TO_TO` (design §8).
    pub create_provider:
        extern "C" fn(
            target: RString,
        )
            -> abi_stable::std_types::RResult<Provider_TO_TO<'static, RBox<()>>, RString>,

    /// Whether `create_provider` can handle `target` (the C++ `canHandle`).
    pub can_handle: extern "C" fn(target: RString) -> bool,

    /// Enumerate processes the provider can attach to (the C++
    /// `enumerateProcesses`); empty if it provides no process list.
    #[sabi(last_prefix_field)]
    pub enumerate_processes: extern "C" fn() -> RVec<AbiProcessInfo>,
}

/// Root-module identity + the load-time version/layout check (design §7.B [+]).
impl RootModule for PluginModRef {
    declare_root_module_statics! {PluginModRef}

    const BASE_NAME: &'static str = "reclass_plugin";
    const NAME: &'static str = "reclass_plugin";
    const VERSION_STRINGS: VersionStrings = package_version_strings!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_round_trips_fields() {
        let m = AbiManifest {
            name: "Example".into(),
            version: "0.1.0".into(),
            author: "me".into(),
            description: "d".into(),
            kind: "native".into(),
            permissions: vec![RString::from("read_memory")].into(),
        };
        assert_eq!(m.name.as_str(), "Example");
        assert_eq!(m.permissions.len(), 1);
        assert_eq!(m.permissions[0].as_str(), "read_memory");
    }

    #[test]
    fn view_tree_nests_like_the_plain_rust_shape() {
        let tree = AbiViewTree::Column(
            vec![
                AbiViewTree::Label("Pick a target".into()),
                AbiViewTree::Group {
                    title: "Process".into(),
                    child: RBox::new(AbiViewTree::Table {
                        id: "procs".into(),
                        columns: vec![RString::from("PID"), RString::from("Name")].into(),
                        rows:
                            vec![vec![RString::from("1234"), RString::from("notepad.exe")].into()]
                                .into(),
                    }),
                },
                AbiViewTree::Separator,
                AbiViewTree::Dropdown {
                    id: "mode".into(),
                    options: vec![RString::from("a"), RString::from("b")].into(),
                    selected: 1,
                },
            ]
            .into(),
        );
        match tree {
            AbiViewTree::Column(children) => {
                assert_eq!(children.len(), 4);
                assert!(matches!(children[1], AbiViewTree::Group { .. }));
            }
            _ => panic!("expected a column root"),
        }
    }

    #[test]
    fn command_result_default_is_unhandled() {
        let r = AbiCommandResult::default();
        assert!(!r.handled);
        assert!(matches!(r.toast, ROption::RNone));
    }

    #[test]
    fn ui_event_and_contribution_variants() {
        let ev = AbiUiEvent::RowSelected {
            table: "procs".into(),
            row: 3,
        };
        assert!(matches!(ev, AbiUiEvent::RowSelected { row: 3, .. }));

        let c = AbiContribution::Command {
            id: "x.go".into(),
            title: "Go".into(),
            slot: AbiCommandSlot::Palette,
        };
        match c {
            AbiContribution::Command { slot, .. } => assert_eq!(slot, AbiCommandSlot::Palette),
            _ => panic!("expected a command"),
        }
    }
}
