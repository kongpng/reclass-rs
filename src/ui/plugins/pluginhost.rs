//! `LivePluginHost` — the window-backed [`PluginHost`] the running app hands a
//! plugin so its callbacks reach the real document area + settings store (design
//! §2; the F2 deliverable — the first *live* host, where only the always-compiled
//! [`MockPluginHost`](crate::plugin::MockPluginHost) existed before).
//!
//! Built transiently inside a `&mut MainWindow` method (the Manage-Plugins
//! dialog's safe-unload path), it borrows the window's [`DocumentArea`] entity,
//! the shared [`DiskSettings`] store, the live `Window`, and the `App` for the
//! duration of one plugin call sequence, then is dropped. It implements the
//! controlled callback surface of design §2:
//!
//! - **read_provider / provider_name** — read the active editor's attached
//!   [`Provider`](crate::provider::Provider) (`controller().document().provider`),
//!   best-effort `false`/`""` when no document/editor is active.
//! - **show_toast** — *collected*, not shown inline: a toast wants
//!   `MainWindow::notify` (which needs `&mut MainWindow`), but the host already
//!   holds `&mut App`/`&mut Window`, so it pushes onto a `Vec` the caller drains
//!   into `notify` after the host drops (design §7.A [fix] — surfaced feedback).
//! - **get_setting / set_setting** — namespaced (`plugin.setting.<key>`) scalars
//!   in the same `settings.json`-backed store the rest of the window persists to
//!   (design §2 "persist plugin settings").
//! - **detach_documents_using** — the safe-unload [fix]: map the provider
//!   `identifier` to the [`SourceKind`] its built-in correspondence implies and
//!   close every document tab of that kind via
//!   [`DocumentArea::detach_sources_of_kind`], returning the count (design §7.A
//!   [fix]; the C++ dangling-provider crash this fixes, cpp_reference §2/§10.3).
//! - **open_dialog / request_rerender** — *collected* like toasts: each pushes the
//!   requested view id onto a `Vec` the caller drains
//!   ([`take_open_dialog_requests`](LivePluginHost::take_open_dialog_requests) /
//!   [`take_rerender_requests`](LivePluginHost::take_rerender_requests)) after the
//!   host drops, then opens the contributed `Dialog` / re-renders the mounted
//!   `Panel` (the F3 declarative-host step, design §6 Phase 2). The host can't do
//!   it inline — both need `&mut MainWindow` (to reach the dock / modal layer),
//!   which the host doesn't hold.
//! - **close_dialog** — *collected* like `open_dialog`: the window dismisses the
//!   open modal if the plugin asked to close it (the demo's Attach closes its own
//!   dialog this way). `add_node` / `set_data_source` are the default no-ops (live
//!   document mutation + plugin-provider attach are intended-deferred).
//!
//! Gated behind the `ui` feature (it lives in the `ui` module). The plugin
//! contract it implements is always-on core.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{App, Window};

use crate::plugin::PluginHost;
use crate::theme::SettingsStore;
use crate::ui::chrome::tabs::DocumentArea;
use crate::ui::state::SourceKind;
use crate::ui::window::DiskSettings;

/// The namespaced prefix for a plugin's persisted scalar settings, completed with
/// the plugin's key: `plugin.setting.<key>`. Kept distinct from the manager's
/// `plugin.enabled.` / `plugin.loadPaths` keys so a plugin can't collide with the
/// host's own plugin bookkeeping.
pub const SETTING_PREFIX: &str = "plugin.setting.";

/// Map a provider `identifier` to the [`SourceKind`] a document attached to it
/// would carry (the safe-unload detach correspondence; design §7.A [fix]).
///
/// `DataSource`/`SourceKind` (`state.rs`) is UI chrome with a fixed enum and
/// carries **no** provider-identifier string, so the live detach maps the
/// identifier through the built-in id→kind correspondence. The built-in
/// identifiers are the derived names of the in-tree providers (`derive_identifier`
/// of "File"/"Buffer"/"Snapshot"/the live process-family providers). `null`
/// attaches nothing (the placeholder provider), so it has no kind. A third-party
/// provider whose identifier is none of these maps to `None`; built-ins map to
/// the same source kind the tab chrome uses.
pub fn source_kind_for_provider(identifier: &str) -> Option<SourceKind> {
    match identifier {
        "file" => Some(SourceKind::File),
        "buffer" => Some(SourceKind::Buffer),
        "snapshot" => Some(SourceKind::Snapshot),
        "processmemory"
        | "remoteprocessmemory"
        | "kernelmemory"
        | "windbgmemory"
        | "memflowprocessmemory" => Some(SourceKind::Process),
        _ => None,
    }
}

/// The gpui-free request-collection buffers a [`LivePluginHost`] accumulates
/// during one plugin call sequence (design §6 Phase 2). The host can't act on a
/// `show_toast` / `open_dialog` / `request_rerender` inline — each needs
/// `&mut MainWindow` (the notification / modal / dock layer) which the host
/// doesn't hold — so it records them here and the caller drains them after the
/// host drops. Factored out of the gpui-bound host so the collection + drain
/// semantics are unit-testable without a `Window`/`App`.
#[derive(Default)]
pub struct HostRequests {
    /// Toasts to surface via `MainWindow::notify`.
    toasts: Vec<String>,
    /// Contributed `Dialog` ids to open via `MainWindow::open_plugin_dialog`.
    open_dialogs: Vec<String>,
    /// Contributed `Dialog` ids the plugin asked to dismiss via
    /// [`PluginHost::close_dialog`] — the window closes the open modal if one of
    /// these matches it (the demo's Attach closes its own dialog this way).
    close_dialogs: Vec<String>,
    /// Mounted view ids to re-render via `MainWindow::rerender_plugin_panel`.
    rerenders: Vec<String>,
}

impl HostRequests {
    fn record_toast(&mut self, msg: &str) {
        self.toasts.push(msg.to_string());
    }
    fn record_open_dialog(&mut self, id: &str) {
        self.open_dialogs.push(id.to_string());
    }
    fn record_close_dialog(&mut self, id: &str) {
        self.close_dialogs.push(id.to_string());
    }
    fn record_rerender(&mut self, view: &str) {
        self.rerenders.push(view.to_string());
    }

    /// Drain the collected toasts.
    pub fn take_toasts(&mut self) -> Vec<String> {
        std::mem::take(&mut self.toasts)
    }
    /// Drain the collected `open_dialog` requests.
    pub fn take_open_dialogs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.open_dialogs)
    }
    /// Drain the collected `close_dialog` requests.
    pub fn take_close_dialogs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.close_dialogs)
    }
    /// Drain the collected `request_rerender` view ids.
    pub fn take_rerenders(&mut self) -> Vec<String> {
        std::mem::take(&mut self.rerenders)
    }
}

/// A window-backed [`PluginHost`] (design §2). Borrows the window's document area
/// + settings store + the live `Window`/`App` for one plugin call sequence.
///
/// Lifetimes: `window` and `cx` are reborrowed mutable references threaded in from
/// the `&mut MainWindow` method that builds the host; the host is constructed,
/// used, and dropped synchronously within that method (it never escapes into an
/// async closure), so the single `'a` over both references is sound.
pub struct LivePluginHost<'a> {
    /// The center document area (cloned `Entity` handle — cheap; the entity lives
    /// in the app). `read`/`update` go through `cx`.
    document_area: gpui::Entity<DocumentArea>,
    /// The shared `settings.json`-backed store the rest of the window persists to,
    /// used for the namespaced `plugin.setting.<key>` scalars.
    settings: Rc<RefCell<DiskSettings>>,
    /// The live window (needed by [`DocumentArea::detach_sources_of_kind`]).
    window: &'a mut Window,
    /// The app context entity `read`/`update` calls thread through.
    cx: &'a mut App,
    /// Toasts / open-dialog / re-render requests the plugin raised during this call
    /// sequence, collected because each needs `&mut MainWindow` (the notification /
    /// modal / dock layer) which the host doesn't hold; the caller drains them via
    /// [`requests`](Self::requests) after the host drops (the F3 host seam).
    requests: HostRequests,
}

impl<'a> LivePluginHost<'a> {
    /// Build the host from the window's pieces. Call inside a `&mut MainWindow`
    /// method: pass `self.document_area.clone()`, `self.settings.clone()`, the
    /// `window`, and `&mut *cx` (the `Context<MainWindow>` derefs to `App`).
    pub fn new(
        document_area: gpui::Entity<DocumentArea>,
        settings: Rc<RefCell<DiskSettings>>,
        window: &'a mut Window,
        cx: &'a mut App,
    ) -> Self {
        LivePluginHost {
            document_area,
            settings,
            window,
            cx,
            requests: HostRequests::default(),
        }
    }

    /// Mutable access to the collected requests so the caller can drain them
    /// (toasts → `notify`, open-dialogs → `open_plugin_dialog`, re-renders →
    /// `rerender_plugin_panel`) after the host drops (the F3 host seam).
    pub fn requests(&mut self) -> &mut HostRequests {
        &mut self.requests
    }

    /// Take the collected toasts (draining them) so the caller can push each into
    /// `MainWindow::notify` after the host drops (kept for the existing safe-unload
    /// caller; equivalent to `self.requests().take_toasts()`).
    pub fn take_toasts(&mut self) -> Vec<String> {
        self.requests.take_toasts()
    }

    /// The full namespaced settings key for a plugin scalar `key`.
    fn setting_key(key: &str) -> String {
        format!("{SETTING_PREFIX}{key}")
    }
}

impl PluginHost for LivePluginHost<'_> {
    fn read_provider(&self, addr: u64, buf: &mut [u8]) -> bool {
        // Read the active editor's attached provider; best-effort false when no
        // document/editor is active (design §2 "read provider bytes").
        let Some(editor) = self.document_area.read(self.cx).active_editor() else {
            return false;
        };
        editor
            .read(self.cx)
            .controller()
            .document()
            .provider
            .read(addr, buf)
    }

    fn provider_name(&self) -> String {
        let Some(editor) = self.document_area.read(self.cx).active_editor() else {
            return String::new();
        };
        editor.read(self.cx).controller().document().provider.name()
    }

    fn show_toast(&mut self, msg: &str) {
        // Collected, not emitted: see the struct doc — the caller drains these
        // into `notify` after the host drops.
        self.requests.record_toast(msg);
    }

    fn open_dialog(&mut self, id: &str) {
        // Collected, not opened inline: mounting a contributed `Dialog` modal needs
        // `&mut MainWindow` (the dock/modal layer), which the host doesn't hold. The
        // caller drains these via `requests().take_open_dialogs()` and calls
        // `open_plugin_dialog` after the host drops (the F3 declarative-host step,
        // design §6 Phase 2). Nothing in the shipping UI changes unless a
        // contributing plugin drives this (HARD PARITY).
        self.requests.record_open_dialog(id);
    }

    fn close_dialog(&mut self, id: &str) {
        // Collected: the window dismisses the open modal if this id matches it
        // (the demo's Attach closes its own dialog). Drained via
        // `requests().take_close_dialogs()` after the host drops.
        self.requests.record_close_dialog(id);
    }

    fn request_rerender(&mut self, view: &str) {
        // Collected, not applied inline: pushing a panel's fresh tree needs
        // `&mut MainWindow` (to reach the mounted `PluginPanel` entity). The caller
        // drains these via `requests().take_rerenders()` and calls
        // `rerender_plugin_panel` after the host drops.
        self.requests.record_rerender(view);
    }

    fn get_setting(&self, key: &str) -> Option<String> {
        self.settings.borrow().get(&Self::setting_key(key))
    }

    fn set_setting(&mut self, key: &str, val: &str) {
        self.settings.borrow_mut().set(&Self::setting_key(key), val);
    }

    fn detach_documents_using(&mut self, identifier: &str) -> usize {
        // The safe-unload [fix]: map the provider identifier to its source kind
        // and close every document of that kind FIRST (design §7.A [fix]). An
        // identifier with no built-in kind correspondence closes nothing.
        let Some(kind) = source_kind_for_provider(identifier) else {
            return 0;
        };
        let window = &mut *self.window;
        self.document_area.update(self.cx, |area, cx| {
            area.detach_sources_of_kind(kind, window, cx)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_kind_maps_builtin_identifiers() {
        // The built-in provider identifiers map to their source kinds; null maps
        // to nothing (it attaches no real source), and an unknown plugin
        // identifier maps to None.
        assert_eq!(source_kind_for_provider("file"), Some(SourceKind::File));
        assert_eq!(source_kind_for_provider("buffer"), Some(SourceKind::Buffer));
        assert_eq!(
            source_kind_for_provider("snapshot"),
            Some(SourceKind::Snapshot)
        );
        assert_eq!(source_kind_for_provider("null"), None);
        assert_eq!(
            source_kind_for_provider("remoteprocessmemory"),
            Some(SourceKind::Process)
        );
        assert_eq!(
            source_kind_for_provider("kernelmemory"),
            Some(SourceKind::Process)
        );
        assert_eq!(
            source_kind_for_provider("windbgmemory"),
            Some(SourceKind::Process)
        );
        assert_eq!(
            source_kind_for_provider("memflowprocessmemory"),
            Some(SourceKind::Process)
        );
        assert_eq!(source_kind_for_provider(""), None);
    }

    #[test]
    fn setting_key_is_namespaced() {
        assert_eq!(
            LivePluginHost::setting_key("dll"),
            "plugin.setting.dll".to_string()
        );
        // Distinct from the manager's enable/loadPaths namespaces (no collision).
        assert!(LivePluginHost::setting_key("enabled.file") != "plugin.enabled.file");
    }

    // ── The F3 request-collection seam ──
    //
    // `LivePluginHost` itself is gpui-bound (it holds `&mut Window`/`&mut App`),
    // so it can't be constructed in a non-gpui test (the repo convention noted in
    // `pluginpanel`/`plugindialog`). The collection + drain semantics it relies on
    // live in the gpui-free `HostRequests`, which we test directly — the host's
    // `show_toast`/`open_dialog`/`request_rerender` are one-line `record_*` calls
    // into this exact buffer, and the live wiring is routed through it.

    #[test]
    fn host_requests_collect_and_drain() {
        let mut reqs = HostRequests::default();
        assert!(reqs.take_toasts().is_empty());
        assert!(reqs.take_open_dialogs().is_empty());
        assert!(reqs.take_rerenders().is_empty());

        reqs.record_toast("hello");
        reqs.record_open_dialog("demo.target");
        reqs.record_open_dialog("demo.other");
        reqs.record_rerender("demo.panel");

        // Each take drains its own buffer independently and in order.
        assert_eq!(reqs.take_toasts(), ["hello".to_string()]);
        assert_eq!(
            reqs.take_open_dialogs(),
            ["demo.target".to_string(), "demo.other".to_string()]
        );
        assert_eq!(reqs.take_rerenders(), ["demo.panel".to_string()]);

        // Draining empties them (a second take is empty) — no double-dispatch.
        assert!(reqs.take_toasts().is_empty());
        assert!(reqs.take_open_dialogs().is_empty());
        assert!(reqs.take_rerenders().is_empty());
    }

    #[test]
    fn demo_round_trips_through_the_request_seam() {
        // The exact Elm round-trip the live path mirrors, but driven through the
        // gpui-free seam: a fake host whose `open_dialog`/`request_rerender` record
        // into a `HostRequests` (the same buffer `LivePluginHost` uses) — proving
        // the demo's command handlers reach the F3 open-dialog + re-render seam.
        use crate::plugin::demo::{DemoPlugin, CMD_OPEN_TARGET, CMD_REFRESH, DIALOG_ID, PANEL_ID};
        use crate::plugin::{Plugin, PluginHost};

        struct SeamHost {
            reqs: HostRequests,
        }
        impl PluginHost for SeamHost {
            fn read_provider(&self, _addr: u64, _buf: &mut [u8]) -> bool {
                false
            }
            fn provider_name(&self) -> String {
                String::new()
            }
            fn show_toast(&mut self, msg: &str) {
                self.reqs.record_toast(msg);
            }
            fn open_dialog(&mut self, id: &str) {
                self.reqs.record_open_dialog(id);
            }
            fn get_setting(&self, _key: &str) -> Option<String> {
                None
            }
            fn set_setting(&mut self, _key: &str, _val: &str) {}
            fn request_rerender(&mut self, view: &str) {
                self.reqs.record_rerender(view);
            }
        }

        let mut plugin = DemoPlugin::new();
        let mut host = SeamHost {
            reqs: HostRequests::default(),
        };
        // open_target → the host's open_dialog seam records the dialog id.
        plugin.handle_command(CMD_OPEN_TARGET, serde_json::Value::Null, &mut host);
        assert_eq!(host.reqs.take_open_dialogs(), [DIALOG_ID.to_string()]);
        // refresh → the host's request_rerender seam records the panel id.
        plugin.handle_command(CMD_REFRESH, serde_json::Value::Null, &mut host);
        assert_eq!(host.reqs.take_rerenders(), [PANEL_ID.to_string()]);
    }
}
