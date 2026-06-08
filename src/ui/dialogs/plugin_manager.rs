//! The Plugins manager dialog (the C++ `showPluginsDialog`) — `PluginInfo`, the
//! `plugin_infos_from_rows` mapping, and `PluginManagerDialog` — extracted from
//! window.rs into the dialogs/ group.

use gpui::prelude::FluentBuilder as _;
use gpui::*;

// ─────────────────────────────────────────────────────────────────────────────
// PluginManagerDialog — the read-only Plugins manager (the C++ showPluginsDialog)
// ─────────────────────────────────────────────────────────────────────────────

/// One row in the Plugins manager (the C++ `IPlugin` descriptor; main.cpp:8845),
/// upgraded to the Phase-6 disclosure model (design §6 Phase 6): beyond the C++
/// name·version·type·author·description it carries the auto-detected **kind
/// label** (design §4), the **enabled** state (design §7.A [fix] — C++ had none),
/// and the declared **permissions** (design §5/§6 disclosure).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginInfo {
    pub(crate) name: String,
    pub(crate) version: String,
    /// The detected-kind label (`builtin` / `native` / `reclassnet-native` /
    /// `reclassnet-managed` / `process`).
    pub(crate) kind: String,
    pub(crate) author: String,
    pub(crate) description: String,
    pub(crate) enabled: bool,
    /// Whether this is a compiled-in built-in provider. Built-ins are **not**
    /// runtime-unloadable (the C++ never `dlopen`'d them; parity), so the dialog
    /// shows no Unload control on a built-in row — only on a loaded native plugin.
    pub(crate) is_builtin: bool,
    /// The derived routing identifier (`Name().toLower().replace(" ","")`) — the
    /// key the dialog hands back to [`PluginManager::set_enabled`] when the user
    /// flips this row's enabled state (design §7.A [fix] / §H).
    pub(crate) identifier: String,
    /// The human-readable permission tokens (e.g. `read_memory`), for disclosure.
    pub(crate) permissions: Vec<String>,
}

/// The plugin rows the Manage Plugins dialog shows, **derived from the real
/// [`PluginManager`](crate::plugin::PluginManager)** via
/// [`plugins_view`](crate::plugin::PluginManager::plugins_view) (design §6 Phase 6:
/// the dialog renders the live plugin set — built-in, native, or auto-detected
/// ReClass.NET — not a second hand-kept table). The C++ `"<name> Provider"`
/// display text is preserved for the built-in providers. Native DLL/SO loading
/// stays out of builds compiled without the `plugins` feature (the dialog notes it).
/// The built-in row list mapped to [`PluginInfo`] — the parity baseline (the
/// live opener reads the session-owned manager instead, but tests assert the shipped
/// built-in set's display fields through this). `#[cfg(test)]` because the only
/// non-test caller now reads the owned manager.
#[cfg(test)]
pub(crate) fn builtin_plugins() -> Vec<PluginInfo> {
    plugin_infos_from_rows(crate::plugin::PluginManager::with_builtins().plugins_view())
}

/// Map a [`PluginManager::plugins_view`](crate::plugin::PluginManager::plugins_view)
/// row list to the dialog's [`PluginInfo`] view-model (design §6 Phase 6). The C++
/// `"<name> Provider"` display text is preserved for built-in providers. Shared by
/// the live opener (reading the session-owned manager) and the parity helper
/// [`builtin_plugins`].
pub(crate) fn plugin_infos_from_rows(rows: Vec<crate::plugin::PluginRow>) -> Vec<PluginInfo> {
    rows.into_iter()
        .map(|r| PluginInfo {
            // Read `is_builtin` before `r.name` is moved by the display branch.
            is_builtin: r.is_builtin,
            name: if r.is_builtin {
                format!("{} Provider", r.name)
            } else {
                r.name
            },
            version: r.version,
            kind: r.detected_label.to_string(),
            author: r.author,
            description: r.description,
            enabled: r.enabled,
            // The derived routing identifier, so the dialog can ask the manager to
            // flip this exact plugin's enabled flag (design §7.A [fix] / §H).
            identifier: r.identifier,
            permissions: r
                .permissions
                .iter()
                .map(|p| p.as_str().to_string())
                .collect(),
        })
        .collect()
}

/// The Plugins manager's outcome. `Close` ends the dialog; `Toggle` asks the host
/// to flip a plugin's enabled flag (design §7.A [fix] / §H — the dialog is no longer
/// read-only: enable/disable drives the session-owned manager + persists). The
/// dialog does not own the manager, so it reports the intent and the window applies
/// it (then pushes the refreshed rows back via [`PluginManagerDialog::set_plugins`]).
#[derive(Clone, Debug)]
pub(crate) enum PluginManagerEvent {
    Close,
    /// Flip `identifier` to `enabled` (the new state the user clicked toward).
    Toggle {
        identifier: String,
        enabled: bool,
    },
    /// **Safe-unload** the plugin `identifier` (design §7.A [fix]). Emitted only by
    /// a NON-builtin row's Unload button (built-ins are never `dlopen`'d, so they
    /// have no Unload control — parity). The host detaches affected documents
    /// FIRST, then drops the plugin + its backing library, then refreshes the rows.
    Unload {
        identifier: String,
    },
    /// Load a native plugin from a user-chosen path (the C++ load-from-path;
    /// design §6 Phase 3/6). Only present + handled behind the `plugins` feature —
    /// no-`plugins` builds ship no runtime loader, so this variant doesn't exist
    /// there (and the dialog shows no "Load plugin..." button).
    #[cfg(feature = "plugins")]
    Load,
}

/// The Plugins manager view (the C++ `showPluginsDialog`; main.cpp:8821). It lists
/// provider/plugin rows with the same fields the C++ shows
/// (name·version·type·author·description). Runtime plugin loading is available
/// only in `plugins` builds; no-`plugins` builds show a clear build-mode note instead.
pub(crate) struct PluginManagerDialog {
    plugins: Vec<PluginInfo>,
    /// Retained native-load failures, `(path, detail)` (design §7.A [fix]). This
    /// state exists only with the runtime plugin loader; no-`plugins` builds have no
    /// native-loader errors to surface.
    #[cfg(feature = "plugins")]
    load_errors: Vec<(std::path::PathBuf, String)>,
    focus_handle: FocusHandle,
}

impl PluginManagerDialog {
    pub(crate) fn new(plugins: Vec<PluginInfo>, cx: &mut Context<Self>) -> Self {
        PluginManagerDialog {
            plugins,
            #[cfg(feature = "plugins")]
            load_errors: Vec::new(),
            focus_handle: cx.focus_handle(),
        }
    }

    /// Install the session manager's retained native-load failures so the dialog
    /// can surface them with detail (design §7.A [fix] — C++ logs then drops the
    /// detail). Feature-gated: builds without `plugins` have no loader, so neither the
    /// caller nor this setter exists there and `load_errors` stays empty.
    #[cfg(feature = "plugins")]
    pub(crate) fn set_load_errors(
        &mut self,
        errs: Vec<(std::path::PathBuf, String)>,
        cx: &mut Context<Self>,
    ) {
        self.load_errors = errs;
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(PluginManagerEvent::Close);
    }

    /// Emit the intent to flip `identifier` to `enabled` (the host owns the manager
    /// and its persistence). The host applies it and pushes the refreshed rows back
    /// via [`set_plugins`](Self::set_plugins), so the chip re-renders from the real
    /// manager state rather than the dialog guessing.
    fn toggle(&mut self, identifier: String, enabled: bool, cx: &mut Context<Self>) {
        cx.emit(PluginManagerEvent::Toggle {
            identifier,
            enabled,
        });
    }

    /// Emit the intent to **safe-unload** `identifier` (design §7.A [fix]). Mirrors
    /// [`toggle`](Self::toggle): the host owns the manager, applies the unload
    /// (detach-first), and pushes the refreshed rows back via
    /// [`set_plugins`](Self::set_plugins). Only a non-builtin row wires this.
    fn unload(&mut self, identifier: String, cx: &mut Context<Self>) {
        cx.emit(PluginManagerEvent::Unload { identifier });
    }

    /// Emit the intent to load a native plugin from a path (the C++ load-from-path;
    /// design §6 Phase 3/6). Feature-gated — builds without `plugins` have no loader, so
    /// neither the button nor this method exists there.
    #[cfg(feature = "plugins")]
    fn load(&mut self, cx: &mut Context<Self>) {
        cx.emit(PluginManagerEvent::Load);
    }

    /// Replace the rendered rows (the host calls this after applying a toggle so the
    /// enabled chip reflects the live [`PluginManager`](crate::plugin::PluginManager)
    /// state).
    pub(crate) fn set_plugins(&mut self, plugins: Vec<PluginInfo>, cx: &mut Context<Self>) {
        self.plugins = plugins;
        cx.notify();
    }
}

impl EventEmitter<PluginManagerEvent> for PluginManagerDialog {}

impl Focusable for PluginManagerDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PluginManagerDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::ui::design::{color, tokens};
        use crate::ui::dialogs::modal;
        use gpui_component::button::{Button, ButtonVariants as _};

        let card_w = modal::clamp_width(620., window);
        let card_max_h = modal::clamp_height(440., 100., window);

        // One card per plugin (the C++ `QListWidgetItem` rows).
        let rows: Vec<AnyElement> = self
            .plugins
            .iter()
            .map(|p| {
                gpui_component::v_flex()
                    .w_full()
                    .gap(px(2.))
                    .p(px(tokens::space::SM))
                    .rounded(px(tokens::radius::SM))
                    .bg(color::panel_bg(cx))
                    .border_1()
                    .border_color(color::border(cx))
                    .child(
                        gpui_component::h_flex()
                            .items_center()
                            .gap(px(tokens::space::SM))
                            .child(
                                div()
                                    .text_color(color::text(cx))
                                    .text_size(px(tokens::font::UI_MD))
                                    .child(format!("{} v{}", p.name, p.version)),
                            )
                            .child(
                                div()
                                    .px(px(tokens::space::XS))
                                    .rounded(px(tokens::radius::SM))
                                    .bg(color::selected_bg(cx))
                                    .text_color(color::text_muted(cx))
                                    .text_size(px(tokens::font::UI_SM))
                                    .child(p.kind.clone()),
                            )
                            // Enabled/disabled chip (design §7.A [fix] — C++ had
                            // no enable state to show).
                            .child(
                                div()
                                    .px(px(tokens::space::XS))
                                    .rounded(px(tokens::radius::SM))
                                    .bg(color::selected_bg(cx))
                                    .text_color(if p.enabled {
                                        color::accent(cx)
                                    } else {
                                        color::text_disabled(cx)
                                    })
                                    .text_size(px(tokens::font::UI_SM))
                                    .child(if p.enabled { "enabled" } else { "disabled" }),
                            )
                            // Push the Enable/Disable control to the right edge.
                            .child(div().flex_grow())
                            // The Enable/Disable toggle (design §7.A [fix] / §H —
                            // the dialog is no longer read-only; the click flips the
                            // session-owned manager + persists). The button reports
                            // the *target* state (`!p.enabled`); the host applies it
                            // and pushes refreshed rows back.
                            .child(
                                Button::new(SharedString::from(format!(
                                    "plugin-toggle-{}",
                                    p.identifier
                                )))
                                .ghost()
                                .label(if p.enabled { "Disable" } else { "Enable" })
                                .on_click(cx.listener({
                                    let id = p.identifier.clone();
                                    let target = !p.enabled;
                                    move |this, _e, _w, cx| {
                                        this.toggle(id.clone(), target, cx);
                                    }
                                })),
                            )
                            // Unload — only a loaded (non-builtin) plugin shows it
                            // (built-ins are never `dlopen`'d, so there is nothing
                            // to unload; parity). A Destructive button: the click
                            // safe-unloads (detach-first) on the session manager.
                            .when(!p.is_builtin, |row| {
                                row.child(
                                    Button::new(SharedString::from(format!(
                                        "plugin-unload-{}",
                                        p.identifier
                                    )))
                                    .danger()
                                    .label("Unload")
                                    .on_click(cx.listener({
                                        let id = p.identifier.clone();
                                        move |this, _e, _w, cx| {
                                            this.unload(id.clone(), cx);
                                        }
                                    })),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_color(color::text_muted(cx))
                            .text_size(px(tokens::font::UI_SM))
                            .child(p.description.to_string()),
                    )
                    .child(
                        div()
                            .text_color(color::text_muted(cx))
                            .text_size(px(tokens::font::UI_SM))
                            .child(format!("Author: {}", p.author)),
                    )
                    // Declared-permission disclosure (design §5/§6 — native plugins
                    // disclose capabilities; built-ins typically have none).
                    .child(
                        div()
                            .text_color(color::text_muted(cx))
                            .text_size(px(tokens::font::UI_SM))
                            .child(if p.permissions.is_empty() {
                                "Permissions: none".to_string()
                            } else {
                                format!("Permissions: {}", p.permissions.join(", "))
                            }),
                    )
                    .into_any_element()
            })
            .collect();

        // The footer note. With the `plugins` feature the runtime loader is
        // present (so the wording invites Load plugin…); without it, the honest
        // boundary note stays verbatim (the C++ load-from-path has no analogue in
        // a no-`plugins` build).
        #[cfg(feature = "plugins")]
        let note_text = "Enable/Disable is applied to the session and persisted \
             across launches. Use “Load plugin…” to load a native plugin (.so/.dll) \
             from disk; built-in providers cannot be unloaded.";
        #[cfg(not(feature = "plugins"))]
        let note_text = "Enable/Disable is applied to the session and persisted \
             across launches. Loading runtime plugins (DLL/SO) is not supported in \
             this build; the provider plugins above are compiled in.";
        let note = div()
            .text_color(color::text_muted(cx))
            .text_size(px(tokens::font::UI_SM))
            .child(note_text);

        // Native-load failure section (design §7.A [fix] — surface ABI/load errors
        // with detail rather than only logging them, the way the C++ generic "check
        // the console" box did NOT). Gated on the `plugins` feature so a no-`plugins`
        // build (no loader, no errors) emits nothing — byte parity. Rendered ABOVE
        // the footer note, inside the same scroll area.
        #[cfg(feature = "plugins")]
        let error_section: Option<AnyElement> = if self.load_errors.is_empty() {
            None
        } else {
            let header = div()
                .text_color(color::danger(cx))
                .text_size(px(tokens::font::UI_SM))
                .child(format!(
                    "Failed to load {} plugin(s):",
                    self.load_errors.len()
                ));
            let lines: Vec<AnyElement> = self
                .load_errors
                .iter()
                .map(|(path, detail)| {
                    div()
                        .text_color(color::text_muted(cx))
                        .text_size(px(tokens::font::UI_SM))
                        .child(format!("{}: {}", path.display(), detail))
                        .into_any_element()
                })
                .collect();
            Some(
                gpui_component::v_flex()
                    .w_full()
                    .gap(px(tokens::space::XS))
                    .child(header)
                    .children(lines)
                    .into_any_element(),
            )
        };

        let body_col = gpui_component::v_flex()
            .id("rcx-plugin-rows")
            .w_full()
            .max_h(px(300.))
            .overflow_y_scroll()
            .gap(px(tokens::space::XS))
            .children(rows);
        #[cfg(feature = "plugins")]
        let body_col = body_col.children(error_section);
        let body = modal::body(cx).child(body_col.child(note));

        let footer = modal::footer(cx);
        // "Load plugin…" — the C++ load-from-path, only behind the `plugins`
        // feature (builds without it have no runtime loader). Placed BEFORE Close.
        #[cfg(feature = "plugins")]
        let footer = footer.child(
            Button::new("plugins-load")
                .label("Load plugin…")
                .on_click(cx.listener(|this, _e, _w, cx| this.load(cx))),
        );
        let footer = footer.child(
            Button::new("plugins-close")
                .primary()
                .label("Close")
                .on_click(cx.listener(|this, _e, _w, cx| this.close(cx))),
        );

        modal::card(cx)
            .id("rcx-plugins")
            .track_focus(&self.focus_handle)
            .key_context("RcxPlugins")
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
                if ev.keystroke.key.as_str() == "escape" {
                    this.close(cx);
                    cx.stop_propagation();
                }
            }))
            .w(card_w)
            .max_h(card_max_h)
            .child(modal::header_with_close(
                "Plugins",
                "plugins-x",
                cx.listener(|this, _e, _w, cx| this.close(cx)),
                cx,
            ))
            .child(body)
            .child(footer)
    }
}
