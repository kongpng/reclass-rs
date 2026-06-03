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
//! - **open_dialog / close_dialog / add_node / set_data_source / request_rerender**
//!   — F2 no-ops (the default [`PluginHost`] trait impls); the modal/tree wiring
//!   is the F3 declarative-host step (design §6 Phase 2).
//!
//! Gated behind the `ui` feature (it lives in the `ui` module). The plugin
//! contract it implements is always-on core.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{App, Window};

use super::state::SourceKind;
use super::tabs::DocumentArea;
use super::window::DiskSettings;
use crate::plugin::PluginHost;
use crate::theme::SettingsStore;

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
/// of "File"/"Buffer"/"Snapshot"). `null` attaches nothing (the placeholder
/// provider), so it has no kind. A third-party native provider whose identifier
/// is none of these maps to `None` — the live detach then closes nothing, which
/// is correct today because live plugin-provider *attach* is intended-deferred
/// (providers are stubs), so no document is ever pointed at one. The mechanism is
/// real and testable against [`DocumentArea`] directly.
pub fn source_kind_for_provider(identifier: &str) -> Option<SourceKind> {
    match identifier {
        "file" => Some(SourceKind::File),
        "buffer" => Some(SourceKind::Buffer),
        "snapshot" => Some(SourceKind::Snapshot),
        _ => None,
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
    /// Toasts the plugin asked to show, collected here because emitting them needs
    /// `&mut MainWindow` (via `notify`) which the host doesn't hold; the caller
    /// [`drains`](Self::take_toasts) them into `notify` after the host drops.
    toasts: Vec<String>,
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
            toasts: Vec::new(),
        }
    }

    /// Take the collected toasts (draining them) so the caller can push each into
    /// `MainWindow::notify` after the host drops (the toast-collection seam).
    pub fn take_toasts(&mut self) -> Vec<String> {
        std::mem::take(&mut self.toasts)
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
        self.toasts.push(msg.to_string());
    }

    fn open_dialog(&mut self, _id: &str) {
        // F2 no-op: the modal machinery (mounting a contributed `Dialog`) is the
        // F3 declarative-host step (design §6 Phase 2). A plugin that requests a
        // dialog before then simply gets no modal; nothing in the shipping UI
        // changes (HARD PARITY — no plugin UI unless a contributing plugin drives
        // the F3 host).
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
        // to nothing (it attaches no real source), and an unknown native plugin
        // identifier maps to None (the live detach then closes nothing — correct
        // while plugin-provider attach is intended-deferred).
        assert_eq!(source_kind_for_provider("file"), Some(SourceKind::File));
        assert_eq!(source_kind_for_provider("buffer"), Some(SourceKind::Buffer));
        assert_eq!(
            source_kind_for_provider("snapshot"),
            Some(SourceKind::Snapshot)
        );
        assert_eq!(source_kind_for_provider("null"), None);
        assert_eq!(source_kind_for_provider("remoteprocessmemory"), None);
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
}
