//! The light plugin **contract** (design §2) — the host-side `Plugin` trait, the
//! `Contribution` kinds it can register, and the small supporting types.
//!
//! ## Light by design
//!
//! Design §2 specifies one host-side contract behind which loaders are an
//! implementation detail. Phase 1 keeps it **dependency-free**: plain Rust types,
//! `serde_json::Value` for command args (already a core dep), and the existing
//! [`Provider`](crate::provider::Provider) trait for the memory-reading surface.
//! No `abi_stable` here — the design (§2 "NO abi_stable in Phase 1") defers the
//! stable-ABI wrapping (`RBox`/`RVec`/`RString`, `#[sabi_trait]`) to Phase 3,
//! behind the `plugins` cargo feature, so lean builds can omit those deps.
//!
//! C++ anchors: `iplugin.h` (`IPlugin`/`IProviderPlugin`), `pluginmanager.cpp`
//! (load + auto-register), captured in `_design/plugin_system_cpp_reference.md`.

use serde_json::Value;

use crate::plugin::host::PluginHost;
use crate::plugin::manifest::PluginManifest;
use crate::plugin::provider_spec::ProviderSpec;
use crate::plugin::view::{UiEvent, ViewTree};

/// The host-side plugin trait (design §2). Object-safe so the host can hold
/// `Box<dyn Plugin>`. `Send + Sync` because providers cross threads (the design
/// §7.B off-GUI-thread read plumbing) and the manager may be shared.
///
/// Every method except [`manifest`](Plugin::manifest) and
/// [`contributions`](Plugin::contributions) has a default, so a pure provider
/// plugin (the Phase 1 built-ins) implements only those two.
pub trait Plugin: Send + Sync {
    /// The plugin's static metadata (design §5). The routing identifier is
    /// *derived* from `manifest().name` via
    /// [`derive_identifier`](crate::plugin::manifest::derive_identifier).
    fn manifest(&self) -> &PluginManifest;

    /// What this plugin contributes to the host (design §2). Read once at
    /// registration; the manager fans these out (providers → `ProviderRegistry`,
    /// UI kinds → the Phase 2 declarative host).
    fn contributions(&self) -> Vec<Contribution>;

    /// Called when the plugin is activated (design §2 lifecycle). Default no-op so
    /// passive provider plugins need not implement it.
    fn activate(&mut self, _host: &mut dyn PluginHost) {}

    /// Called before the plugin is disabled/unloaded. Default no-op.
    fn deactivate(&mut self) {}

    /// Handle a `Command` contribution being invoked (design §2/§3). `args` is
    /// opaque JSON the host passes through. Default: not handled.
    fn handle_command(
        &mut self,
        _id: &str,
        _args: Value,
        _host: &mut dyn PluginHost,
    ) -> CommandResult {
        CommandResult::default()
    }

    /// Handle a UI event from a contributed `Panel`/`Dialog`/`StatusItem`
    /// (design §3, Elm-style). Returning `Some(tree)` re-renders that view.
    /// Default: ignore (no re-render).
    fn handle_ui_event(
        &mut self,
        _view: &str,
        _ev: UiEvent,
        _host: &mut dyn PluginHost,
    ) -> Option<ViewTree> {
        None
    }

    /// A contributed `Dialog` (the generalized C++ `selectTarget`, design §3)
    /// closed — report its outcome back to the plugin. `view` is the dialog id;
    /// `result` is [`DialogResult::Submitted`] (with the collected field values)
    /// or [`DialogResult::Cancelled`]. Default no-op so no existing impl breaks —
    /// only plugins that own a `Dialog` need react (e.g. set the data source on a
    /// submitted target picker).
    fn handle_dialog_closed(
        &mut self,
        _view: &str,
        _result: DialogResult,
        _host: &mut dyn PluginHost,
    ) -> CommandResult {
        CommandResult::default()
    }
}

/// The outcome a contributed `Dialog` reports back via
/// [`Plugin::handle_dialog_closed`] (design §3 — the generalized C++
/// `selectTarget`, which returned the chosen target string or nothing). A
/// submitted dialog carries the `(field_id, value)` pairs the host collected
/// from the dialog's inputs/selections; a cancelled dialog carries nothing (the
/// C++ `selectTarget` returning an empty string / `reject`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogResult {
    /// The dialog was confirmed; `values` are the `(field_id, value)` pairs the
    /// host gathered from its inputs/dropdowns/selected rows.
    Submitted { values: Vec<(String, String)> },
    /// The dialog was dismissed (Esc / Cancel / `×`).
    Cancelled,
}

impl DialogResult {
    /// Whether this is a [`DialogResult::Submitted`].
    pub fn is_submitted(&self) -> bool {
        matches!(self, DialogResult::Submitted { .. })
    }

    /// The value submitted for `id`, if this is a `Submitted` result that carries
    /// it (a small convenience so a plugin can pull a field without matching).
    pub fn get(&self, id: &str) -> Option<&str> {
        match self {
            DialogResult::Submitted { values } => values
                .iter()
                .find(|(k, _)| k == id)
                .map(|(_, v)| v.as_str()),
            DialogResult::Cancelled => None,
        }
    }
}

/// What a plugin contributes to the host (design §2). `Provider` is the Phase 1
/// path (full C++ `IProviderPlugin` parity); the UI kinds are the design §3
/// "add UI" surfaces, rendered by the Phase 2 declarative host.
pub enum Contribution {
    /// A data source (design §2 — Goal 1, C++ `IProviderPlugin`). Carries the
    /// [`ProviderSpec`] that can `can_handle`/`create_provider`.
    Provider(ProviderSpec),
    /// A command surfaced in one of the [`CommandSlot`]s (design §3; generalizes
    /// + covers C++ `populatePluginMenu`).
    Command {
        id: String,
        title: String,
        slot: CommandSlot,
    },
    /// A dockable panel rendered from a declarative [`ViewTree`] (design §3).
    Panel {
        id: String,
        title: String,
        dock: DockSide,
        initial: ViewTree,
    },
    /// A modal dialog (design §3) — this is the generalized C++ `selectTarget`.
    Dialog {
        id: String,
        title: String,
        initial: ViewTree,
    },
    /// A status-bar item (design §3).
    StatusItem { id: String, initial: ViewTree },
}

impl Contribution {
    /// The opaque id of this contribution, if it has one (everything but a
    /// `Provider`, whose identity is its derived plugin identifier).
    pub fn id(&self) -> Option<&str> {
        match self {
            Contribution::Provider(_) => None,
            Contribution::Command { id, .. }
            | Contribution::Panel { id, .. }
            | Contribution::Dialog { id, .. }
            | Contribution::StatusItem { id, .. } => Some(id),
        }
    }

    /// Whether this contribution is a [`Contribution::Provider`].
    pub fn is_provider(&self) -> bool {
        matches!(self, Contribution::Provider(_))
    }

    /// Whether this contribution is a host-rendered **view** — a `Panel`,
    /// `Dialog`, or `StatusItem` (design §3), i.e. something the Phase-2
    /// declarative renderer mounts and routes [`UiEvent`]s for. A `Command`
    /// surfaces in a menu/palette (no `ViewTree` of its own) and a `Provider`
    /// has no UI, so neither is a view.
    pub fn is_view(&self) -> bool {
        matches!(
            self,
            Contribution::Panel { .. }
                | Contribution::Dialog { .. }
                | Contribution::StatusItem { .. }
        )
    }
}

impl Contribution {
    /// The ids of the renderable **views** in a contribution list — every
    /// `Panel`/`Dialog`/`StatusItem` id, in order (design §3). The Phase-2
    /// declarative renderer enumerates these to know which views to mount and
    /// which ids a routed [`UiEvent`]/[`DialogResult`] can target. `Command`s and
    /// `Provider`s are excluded ([`is_view`](Contribution::is_view)).
    pub fn view_ids(contributions: &[Contribution]) -> Vec<&str> {
        contributions
            .iter()
            .filter(|c| c.is_view())
            .filter_map(|c| c.id())
            .collect()
    }
}

/// Where a [`Contribution::Command`] is surfaced (design §3 surfaces). Covers and
/// generalizes the single C++ `populatePluginMenu` injection point
/// (cpp_reference §3 — plugins inject items like WinDbg "Unload Driver" into the
/// source menu, which is the [`CommandSlot::SourceMenu`] case).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandSlot {
    /// The main menu bar.
    Menu,
    /// The node editor's right-click context menu.
    EditorContext,
    /// The File ▸ Data Source menu (C++ `populatePluginMenu`).
    SourceMenu,
    /// A toolbar button.
    Toolbar,
    /// The command palette.
    Palette,
}

/// Which edge a [`Contribution::Panel`] docks to (design §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockSide {
    Left,
    Right,
    Bottom,
}

/// The outcome of [`Plugin::handle_command`] (design §2). `handled = false` lets
/// the host fall through to its own handling; an optional `toast` surfaces a
/// short message (design §7.A [fix] — structured, surfaced feedback instead of a
/// "check the console" box).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandResult {
    pub handled: bool,
    pub toast: Option<String>,
}

impl CommandResult {
    /// A handled command with no message.
    pub fn handled() -> Self {
        CommandResult {
            handled: true,
            toast: None,
        }
    }

    /// A handled command that shows `msg` as a toast.
    pub fn toast(msg: impl Into<String>) -> Self {
        CommandResult {
            handled: true,
            toast: Some(msg.into()),
        }
    }
}

/// A process the host can attach to (design §2; mirrors C++ `PluginProcessInfo`
/// `iplugin.h:65-75`, minus the `QIcon` — icons are a host concern). Returned by
/// [`ProviderSpec::enumerate_processes`](crate::plugin::provider_spec::ProviderSpec::enumerate_processes)
/// and rendered by the host's (Phase 2 declarative) process picker.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub path: String,
    /// 32-bit target (the C++ `is32Bit`; drives the " (32-bit)" name suffix).
    pub is_32bit: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::host::MockPluginHost;
    use crate::plugin::manifest::{Permission, PluginManifest};

    /// A minimal plugin to prove the trait is object-safe and usable as
    /// `Box<dyn Plugin>` with only the two required methods.
    struct DummyPlugin {
        manifest: PluginManifest,
    }

    impl Plugin for DummyPlugin {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }
        fn contributions(&self) -> Vec<Contribution> {
            vec![
                Contribution::Command {
                    id: "demo.hello".to_string(),
                    title: "Hello".to_string(),
                    slot: CommandSlot::Palette,
                },
                Contribution::StatusItem {
                    id: "demo.status".to_string(),
                    initial: ViewTree::Label("ready".to_string()),
                },
            ]
        }
        fn handle_command(
            &mut self,
            id: &str,
            _args: Value,
            host: &mut dyn PluginHost,
        ) -> CommandResult {
            if id == "demo.hello" {
                host.show_toast("hi");
                CommandResult::toast("hi")
            } else {
                CommandResult::default()
            }
        }
    }

    #[test]
    fn plugin_is_object_safe_and_dispatches() {
        let mut p: Box<dyn Plugin> = Box::new(DummyPlugin {
            manifest: PluginManifest::builtin("Demo", "demo", vec![Permission::AddUi]),
        });
        assert_eq!(p.manifest().identifier(), "demo");

        let contribs = p.contributions();
        assert_eq!(contribs.len(), 2);
        assert_eq!(contribs[0].id(), Some("demo.hello"));
        assert!(!contribs[0].is_provider());

        // Default methods are callable on the trait object.
        let mut host = MockPluginHost::new();
        p.activate(&mut host);
        let res = p.handle_command("demo.hello", Value::Null, &mut host);
        assert!(res.handled);
        assert_eq!(res.toast.as_deref(), Some("hi"));
        assert_eq!(host.toasts(), ["hi"]);

        // Unknown command falls through to the default (not handled).
        let res = p.handle_command("demo.unknown", Value::Null, &mut host);
        assert!(!res.handled);
        // No UI event handler → no re-render by default.
        assert_eq!(
            p.handle_ui_event("demo.status", UiEvent::Clicked("x".into()), &mut host),
            None
        );
        p.deactivate();
    }

    #[test]
    fn command_result_constructors() {
        assert_eq!(
            CommandResult::default(),
            CommandResult {
                handled: false,
                toast: None
            }
        );
        assert!(CommandResult::handled().handled);
        assert_eq!(CommandResult::toast("ok").toast.as_deref(), Some("ok"));
    }

    #[test]
    fn process_info_mirrors_cpp_shape() {
        let pi = ProcessInfo {
            pid: 4321,
            name: "game.exe".to_string(),
            path: "/games/game.exe".to_string(),
            is_32bit: true,
        };
        assert_eq!(pi.pid, 4321);
        assert!(pi.is_32bit);
    }

    #[test]
    fn contribution_id_and_provider_predicate() {
        let dialog = Contribution::Dialog {
            id: "pick".to_string(),
            title: "Pick".to_string(),
            initial: ViewTree::Separator,
        };
        assert_eq!(dialog.id(), Some("pick"));
        assert!(!dialog.is_provider());
        assert!(dialog.is_view());
    }

    #[test]
    fn view_ids_enumerates_panels_dialogs_statusitems_only() {
        let contributions = vec![
            Contribution::Command {
                id: "demo.ping".to_string(),
                title: "Ping".to_string(),
                slot: CommandSlot::Menu,
            },
            Contribution::Panel {
                id: "demo.panel".to_string(),
                title: "Panel".to_string(),
                dock: DockSide::Right,
                initial: ViewTree::Separator,
            },
            Contribution::Dialog {
                id: "demo.target".to_string(),
                title: "Target".to_string(),
                initial: ViewTree::Separator,
            },
            Contribution::StatusItem {
                id: "demo.status".to_string(),
                initial: ViewTree::Label("ok".to_string()),
            },
        ];
        // Commands are excluded; the three view kinds enumerate in order.
        assert_eq!(
            Contribution::view_ids(&contributions),
            ["demo.panel", "demo.target", "demo.status"]
        );
        // is_view agrees per-variant.
        assert!(!contributions[0].is_view());
        assert!(contributions[1].is_view());
        assert!(contributions[2].is_view());
        assert!(contributions[3].is_view());
    }

    #[test]
    fn dialog_result_helpers() {
        let submitted = DialogResult::Submitted {
            values: vec![
                ("target".to_string(), "1234:notepad.exe".to_string()),
                ("live".to_string(), "true".to_string()),
            ],
        };
        assert!(submitted.is_submitted());
        assert_eq!(submitted.get("target"), Some("1234:notepad.exe"));
        assert_eq!(submitted.get("missing"), None);

        let cancelled = DialogResult::Cancelled;
        assert!(!cancelled.is_submitted());
        assert_eq!(cancelled.get("target"), None);
    }

    #[test]
    fn handle_dialog_closed_defaults_to_unhandled() {
        // The new default method is callable on a trait object and, for a plugin
        // that doesn't override it, returns the not-handled default (no break).
        let mut p: Box<dyn Plugin> = Box::new(DummyPlugin {
            manifest: PluginManifest::builtin("Demo", "demo", vec![Permission::AddUi]),
        });
        let mut host = MockPluginHost::new();
        let res = p.handle_dialog_closed(
            "demo.target",
            DialogResult::Submitted {
                values: vec![("target".to_string(), "x".to_string())],
            },
            &mut host,
        );
        assert!(!res.handled);
        let res = p.handle_dialog_closed("demo.target", DialogResult::Cancelled, &mut host);
        assert!(!res.handled);
    }
}
