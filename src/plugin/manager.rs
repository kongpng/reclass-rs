//! `PluginManager` — owns the loaded plugins and auto-registers their `Provider`
//! contributions into the existing [`ProviderRegistry`] (design §6 Phase 1; the
//! C++ `PluginManager`, cpp_reference §2).
//!
//! C++ parity: on load, a provider plugin is auto-registered under
//! `identifier = Name().toLower().replace(" ","")` (cpp_reference §2). We do the
//! same, but route the identifier through the single
//! [`derive_identifier`](crate::plugin::manifest::derive_identifier) helper
//! (design §7.A [fix]) and keep the [`ProviderSpec`] so an identifier can be
//! mapped back to a real [`create_provider`](ProviderSpec::create_provider) at
//! attach time (the C++ `selectSource`/`attachViaPlugin` path, cpp_reference §5).
//!
//! Phase 1 loads only the in-tree built-ins ([`with_builtins`](PluginManager::with_builtins));
//! the native/`abi_stable` and ReClass.NET loaders (design §6 Phase 3/4) drop in
//! behind the `plugins` feature without changing this registration flow.

use std::collections::HashMap;

use serde_json::Value;

use crate::plugin::builtins;
use crate::plugin::contract::{CommandResult, Contribution, DialogResult, Plugin};
use crate::plugin::host::PluginHost;
use crate::plugin::provider_spec::{ProviderSpec, SharedProvider};
use crate::plugin::view::{UiEvent, ViewTree};
use crate::provider::{ProviderInfo, ProviderRegistry};

/// Owns the loaded plugins + the provider registry they populate (the C++
/// `MainWindow::m_pluginManager`, cpp_reference §2). One per application.
#[derive(Default)]
pub struct PluginManager {
    plugins: Vec<Box<dyn Plugin>>,
    registry: ProviderRegistry,
    /// identifier → index of the plugin in `plugins` (so we can fetch its
    /// `ProviderSpec` to create a provider at attach time). Built during
    /// registration; the C++ re-finds the plugin by recomputed identifier.
    provider_index: HashMap<String, usize>,
    /// command id → owning plugin index (design §6 Phase 2 routing — so a
    /// dispatched `Command` reaches the plugin that contributed it).
    command_index: HashMap<String, usize>,
    /// view id (`Panel`/`Dialog`/`StatusItem`) → owning plugin index, so a
    /// routed [`UiEvent`]/[`DialogResult`] reaches the contributing plugin.
    view_index: HashMap<String, usize>,
}

impl PluginManager {
    /// An empty manager (no plugins, empty registry).
    pub fn new() -> Self {
        PluginManager::default()
    }

    /// Build a manager with the four in-tree built-ins loaded + their providers
    /// registered (design §6 Phase 1). This is the Phase-1 replacement for the C++
    /// deferred `LoadPlugins()` (cpp_reference §2) — the built-ins flow through the
    /// contract instead of being wired ad hoc by the menus.
    pub fn with_builtins() -> Self {
        let mut mgr = PluginManager::new();
        for plugin in builtins::builtin_plugins() {
            mgr.add_plugin(plugin);
        }
        mgr
    }

    /// As [`with_builtins`](PluginManager::with_builtins) but also loads the
    /// in-tree [`DemoPlugin`](crate::plugin::demo::DemoPlugin) — the Phase-2
    /// deliverable that contributes a `Command`, a `Panel`, and a `Dialog` for
    /// the declarative-UI host to render (design §6 Phase 2). Kept separate so
    /// [`with_builtins`](PluginManager::with_builtins) stays exactly the
    /// four-provider Phase-1 set (the parity tests rely on that).
    pub fn with_builtins_and_demo() -> Self {
        let mut mgr = PluginManager::with_builtins();
        mgr.add_plugin(crate::plugin::demo::DemoPlugin::boxed());
        mgr
    }

    /// Register a plugin and fan its contributions out (cpp_reference §2
    /// auto-register). Provider contributions are added to the registry under the
    /// plugin's derived identifier; the index records the slot so the spec can be
    /// recovered at attach time. Returns the derived identifier.
    pub fn add_plugin(&mut self, plugin: Box<dyn Plugin>) -> String {
        let manifest = plugin.manifest();
        let identifier = manifest.identifier();
        let name = manifest.name.clone();
        let dll = manifest.dll_file_name.clone();
        let is_builtin = matches!(manifest.kind, crate::plugin::manifest::PluginKind::Builtin);

        let idx = self.plugins.len();
        let mut has_provider = false;
        for contribution in plugin.contributions() {
            match &contribution {
                Contribution::Provider(_) => has_provider = true,
                // UI-kind contributions: index them so the Phase-2 declarative
                // host can route a dispatched command / UI event / dialog result
                // back to the contributing plugin (design §6 Phase 2).
                Contribution::Command { id, .. } => {
                    self.command_index.insert(id.clone(), idx);
                }
                Contribution::Panel { id, .. }
                | Contribution::Dialog { id, .. }
                | Contribution::StatusItem { id, .. } => {
                    self.view_index.insert(id.clone(), idx);
                }
            }
        }

        if has_provider {
            self.registry.register_provider(ProviderInfo {
                name,
                identifier: identifier.clone(),
                is_builtin,
                dll_file_name: dll,
                enabled: true,
            });
            self.provider_index.insert(identifier.clone(), idx);
        }

        self.plugins.push(plugin);
        identifier
    }

    /// The provider registry both source-picker surfaces read (design §7.A [fix]).
    pub fn registry(&self) -> &ProviderRegistry {
        &self.registry
    }

    /// Mutable registry access (e.g. enable/disable from the Manage Plugins
    /// dialog, design §7.A [fix]).
    pub fn registry_mut(&mut self) -> &mut ProviderRegistry {
        &mut self.registry
    }

    /// All loaded plugins (the C++ `plugins()`, cpp_reference §2).
    pub fn plugins(&self) -> &[Box<dyn Plugin>] {
        &self.plugins
    }

    /// Find a loaded plugin by its derived identifier (the C++ `FindPlugin`, but
    /// keyed on the normalized identifier).
    pub fn find_plugin(&self, identifier: &str) -> Option<&dyn Plugin> {
        self.provider_index
            .get(identifier)
            .map(|&i| self.plugins[i].as_ref())
    }

    /// Fetch the [`ProviderSpec`] registered under `identifier` (the C++
    /// `selectSource` lookup before `createProvider`, cpp_reference §5). Pulls the
    /// (single) provider contribution out of the plugin's `contributions()`.
    pub fn provider_spec(&self, identifier: &str) -> Option<ProviderSpec> {
        let &idx = self.provider_index.get(identifier)?;
        self.plugins[idx]
            .contributions()
            .into_iter()
            .find_map(|c| match c {
                Contribution::Provider(spec) => Some(spec),
                _ => None,
            })
    }

    /// Create a provider for `identifier` + `target` (the C++ attach path:
    /// `findProvider` → `createProvider`, cpp_reference §5). Surfaces the
    /// provider's error string (design §7.A [fix]). Errors if no such provider or
    /// it can't handle the target.
    pub fn create_provider(
        &self,
        identifier: &str,
        target: &str,
    ) -> Result<SharedProvider, String> {
        let spec = self
            .provider_spec(identifier)
            .ok_or_else(|| format!("no provider registered as '{identifier}'"))?;
        spec.create_provider(target)
    }

    // ── Phase-2 declarative-host routing (design §6 Phase 2, §7.B [+]) ──
    //
    // Each entry point is wrapped in `catch_unwind` so a panicking plugin
    // surfaces as "not handled" instead of taking down the host (design §7.B
    // "Wrap every plugin entry point in `catch_unwind`"). The `&mut` borrows are
    // not unwind-safe by default, but a plugin panic here only abandons that one
    // call — the manager keeps the plugin and stays usable — so `AssertUnwindSafe`
    // is sound for our use (we don't read poisoned plugin state afterwards).

    /// Dispatch a contributed `Command` to its owning plugin (design §3). Routes
    /// by the command id recorded at registration; returns the plugin's
    /// [`CommandResult`] (the default not-handled result if no plugin owns `id`
    /// or the handler panicked). Panic-guarded (design §7.B [+]).
    pub fn handle_command(
        &mut self,
        id: &str,
        args: Value,
        host: &mut dyn PluginHost,
    ) -> CommandResult {
        let Some(&idx) = self.command_index.get(id) else {
            return CommandResult::default();
        };
        let plugin = &mut self.plugins[idx];
        guard(CommandResult::default(), || {
            plugin.handle_command(id, args, host)
        })
    }

    /// Route a [`UiEvent`] from a contributed `Panel`/`Dialog`/`StatusItem` to
    /// its owning plugin (design §3 Elm loop). Returns the fresh [`ViewTree`] to
    /// re-render that view, or `None` (unowned view, no re-render, or a panic).
    /// Panic-guarded (design §7.B [+]).
    pub fn handle_ui_event(
        &mut self,
        view: &str,
        ev: UiEvent,
        host: &mut dyn PluginHost,
    ) -> Option<ViewTree> {
        let &idx = self.view_index.get(view)?;
        let plugin = &mut self.plugins[idx];
        guard(None, || plugin.handle_ui_event(view, ev, host))
    }

    /// Report a contributed `Dialog`'s outcome to its owning plugin (the
    /// generalized C++ `selectTarget` return, design §3). Panic-guarded.
    pub fn handle_dialog_closed(
        &mut self,
        view: &str,
        result: DialogResult,
        host: &mut dyn PluginHost,
    ) -> CommandResult {
        let Some(&idx) = self.view_index.get(view) else {
            return CommandResult::default();
        };
        let plugin = &mut self.plugins[idx];
        guard(CommandResult::default(), || {
            plugin.handle_dialog_closed(view, result, host)
        })
    }

    /// Re-ask the plugin that owns `view` for that view's current `ViewTree`
    /// (the [`PluginHost::request_rerender`](crate::plugin::host::PluginHost::request_rerender)
    /// resolution: the host calls this to pull the fresh tree out of the plugin's
    /// `contributions()`). Panic-guarded; `None` if unowned / not a view / panic.
    pub fn view_tree(&self, view: &str) -> Option<ViewTree> {
        let &idx = self.view_index.get(view)?;
        let plugin = self.plugins[idx].as_ref();
        guard(None, || {
            plugin.contributions().into_iter().find_map(|c| match c {
                Contribution::Panel { id, initial, .. } if id == view => Some(initial),
                Contribution::Dialog { id, initial, .. } if id == view => Some(initial),
                Contribution::StatusItem { id, initial } if id == view => Some(initial),
                _ => None,
            })
        })
    }
}

/// Run a plugin entry point with a panic guard (design §7.B [+] — a plugin panic
/// must not crash the host). Returns `fallback` if the closure panics. The
/// closure borrows `&mut` plugin/host, which aren't `UnwindSafe`; we assert it
/// because a panic here abandons only this one call (we don't subsequently read
/// the plugin's now-possibly-inconsistent state).
fn guard<T>(fallback: T, f: impl FnOnce() -> T) -> T {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_builtins_registers_four_providers_in_order() {
        let mgr = PluginManager::with_builtins();
        let ids: Vec<&str> = mgr
            .registry()
            .providers()
            .iter()
            .map(|p| p.identifier.as_str())
            .collect();
        // Registration order = built-in listing order (cpp_reference §2/§3).
        assert_eq!(ids, ["file", "buffer", "snapshot", "null"]);
        // All marked built-in + enabled.
        assert!(mgr
            .registry()
            .providers()
            .iter()
            .all(|p| p.is_builtin && p.enabled));
    }

    #[test]
    fn create_provider_routes_through_the_spec() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("rcx-mgr-test-{}.bin", std::process::id()));
        std::fs::write(&path, [1u8, 2, 3, 4, 5]).unwrap();

        let mgr = PluginManager::with_builtins();
        // The "file" identifier maps back to the FilePlugin's spec.
        let prov = match mgr.create_provider("file", path.to_str().unwrap()) {
            Ok(p) => p,
            Err(e) => panic!("file provider: {e}"),
        };
        assert_eq!(prov.size(), 5);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn create_provider_unknown_identifier_errors() {
        let mgr = PluginManager::with_builtins();
        let err = match mgr.create_provider("doesnotexist", "x") {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        };
        assert!(err.contains("doesnotexist"));
    }

    #[test]
    fn find_plugin_by_identifier() {
        let mgr = PluginManager::with_builtins();
        assert!(mgr.find_plugin("buffer").is_some());
        assert_eq!(mgr.find_plugin("buffer").unwrap().manifest().name, "Buffer");
        assert!(mgr.find_plugin("nope").is_none());
    }

    #[test]
    fn enable_disable_via_registry_mut() {
        let mut mgr = PluginManager::with_builtins();
        assert!(mgr.registry_mut().set_enabled("null", false));
        let enabled: Vec<&str> = mgr
            .registry()
            .enabled_providers()
            .map(|p| p.identifier.as_str())
            .collect();
        assert_eq!(enabled, ["file", "buffer", "snapshot"]);
    }

    // ── Phase-2 routing ──

    use crate::plugin::demo;
    use crate::plugin::host::MockPluginHost;

    #[test]
    fn with_builtins_unchanged_with_demo_added_separately() {
        // Parity: with_builtins is still exactly the four-provider set.
        let plain = PluginManager::with_builtins();
        assert_eq!(plain.registry().enabled_providers().count(), 4);
        assert!(plain.find_plugin("plugindemo").is_none());

        // The demo constructor adds the demo plugin (one more plugin, but it
        // contributes no provider, so the registry is still the four built-ins).
        let demo_mgr = PluginManager::with_builtins_and_demo();
        assert_eq!(demo_mgr.registry().enabled_providers().count(), 4);
        assert_eq!(demo_mgr.plugins().len(), 5);
    }

    #[test]
    fn routes_command_to_owning_plugin() {
        let mut mgr = PluginManager::with_builtins_and_demo();
        let mut host = MockPluginHost::new();
        let res = mgr.handle_command(demo::CMD_PING, serde_json::Value::Null, &mut host);
        assert!(res.handled);
        assert_eq!(host.toasts(), ["Plugin Demo: pong"]);
    }

    #[test]
    fn unknown_command_is_not_handled() {
        let mut mgr = PluginManager::with_builtins_and_demo();
        let mut host = MockPluginHost::new();
        let res = mgr.handle_command("nope.nothing", serde_json::Value::Null, &mut host);
        assert!(!res.handled);
    }

    #[test]
    fn routes_ui_event_and_returns_fresh_tree() {
        let mut mgr = PluginManager::with_builtins_and_demo();
        let mut host = MockPluginHost::new();
        // Attach button on the demo dialog: sets the data source + closes it.
        let tree = mgr.handle_ui_event(
            demo::DIALOG_ID,
            crate::plugin::view::UiEvent::Clicked(demo::BTN_ATTACH.to_string()),
            &mut host,
        );
        assert!(tree.is_some());
        assert_eq!(host.closed_dialogs(), [demo::DIALOG_ID]);
        assert!(host.data_source().is_some());
    }

    #[test]
    fn ui_event_for_unowned_view_is_none() {
        let mut mgr = PluginManager::with_builtins_and_demo();
        let mut host = MockPluginHost::new();
        assert_eq!(
            mgr.handle_ui_event(
                "no.such.view",
                crate::plugin::view::UiEvent::Clicked("x".to_string()),
                &mut host
            ),
            None
        );
    }

    #[test]
    fn view_tree_pulls_current_panel_tree() {
        let mgr = PluginManager::with_builtins_and_demo();
        let tree = mgr.view_tree(demo::PANEL_ID).expect("panel tree");
        assert!(matches!(tree, crate::plugin::view::ViewTree::Column(_)));
        assert!(mgr.view_tree("no.such.view").is_none());
    }

    #[test]
    fn routes_dialog_closed_to_plugin() {
        let mut mgr = PluginManager::with_builtins_and_demo();
        let mut host = MockPluginHost::new();
        let res = mgr.handle_dialog_closed(
            demo::DIALOG_ID,
            crate::plugin::contract::DialogResult::Submitted {
                values: vec![(demo::FIELD_TARGET.to_string(), "7:x.exe".to_string())],
            },
            &mut host,
        );
        assert!(res.handled);
        assert_eq!(
            host.data_source(),
            Some(&(demo::DEMO_IDENTIFIER.to_string(), "7:x.exe".to_string()))
        );
    }

    #[test]
    fn panicking_plugin_is_contained() {
        // A plugin whose command handler panics must not crash the host: the
        // guard returns the not-handled fallback (design §7.B [+]).
        struct PanicPlugin {
            manifest: crate::plugin::manifest::PluginManifest,
        }
        impl crate::plugin::contract::Plugin for PanicPlugin {
            fn manifest(&self) -> &crate::plugin::manifest::PluginManifest {
                &self.manifest
            }
            fn contributions(&self) -> Vec<Contribution> {
                vec![Contribution::Command {
                    id: "boom.go".to_string(),
                    title: "Boom".to_string(),
                    slot: crate::plugin::contract::CommandSlot::Menu,
                }]
            }
            fn handle_command(
                &mut self,
                _id: &str,
                _args: serde_json::Value,
                _host: &mut dyn PluginHost,
            ) -> CommandResult {
                panic!("plugin blew up");
            }
        }

        let mut mgr = PluginManager::new();
        mgr.add_plugin(Box::new(PanicPlugin {
            manifest: crate::plugin::manifest::PluginManifest::builtin("Boom", "panics", vec![]),
        }));
        let mut host = MockPluginHost::new();
        // Silence the default panic hook's backtrace noise during this test.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let res = mgr.handle_command("boom.go", serde_json::Value::Null, &mut host);
        std::panic::set_hook(prev);
        // Contained: not handled, host still alive.
        assert!(!res.handled);
    }
}
