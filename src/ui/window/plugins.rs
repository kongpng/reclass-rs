//! Plugin host (design §6 Phase 2) — mounting plugin-contributed panels, the live
//! declarative-UI host, command/panel/dialog dispatch, and the plugin-manager
//! dialog. Extracted from window.rs as an `impl super::MainWindow` cluster (the
//! gpui side of the session-owned `plugin_manager`). With no contributing plugin
//! loaded, every path here is a no-op over an empty list.

// `use super::*` inherits the parent window module's full import set (gpui, the
// crate types, the plugin-manager dialog types) — a child module can see its
// ancestor's private `use` aliases — so no further imports are needed here.
use super::*;

/// Map a plugin [`DockSide`](crate::plugin::DockSide) to the gpui-component
/// [`DockPlacement`] a contributed panel mounts at (design §6 Phase 2). The demo's
/// panel is `Right`, so it tabs in beside the Modules/Bookmarks right dock.
pub(crate) fn dock_placement_for(side: crate::plugin::DockSide) -> DockPlacement {
    match side {
        crate::plugin::DockSide::Left => DockPlacement::Left,
        crate::plugin::DockSide::Right => DockPlacement::Right,
        crate::plugin::DockSide::Bottom => DockPlacement::Bottom,
    }
}

/// A command's own toast, unless the live host already surfaced it (dedup the
/// `CommandResult.toast` against the host's drained toast list).
pub(super) fn surface_command_toast(
    res_toast: Option<String>,
    already: &[String],
) -> Option<String> {
    res_toast.filter(|m| !already.iter().any(|t| t == m))
}

impl super::MainWindow {
    // ── F3 live declarative-UI host (design §6 Phase 2) ──
    //
    // The session-owned `plugin_manager` already routes a command / UI event /
    // dialog result back to its owning plugin (`handle_command` /
    // `handle_ui_event` / `handle_dialog_closed`); these methods are the gpui side
    // that (1) enumerates the manager's `ui_contributions` to mount panels + inject
    // menu items, (2) builds a scoped `LivePluginHost` per call sequence, and (3)
    // drains the host's collected toasts / open-dialog / re-render requests into
    // `notify` / `open_plugin_dialog` / `rerender_plugin_panel`. With NO contributing
    // plugin loaded, every loop here is a no-op over an empty
    // list — nothing is mounted, injected, or routed (HARD PARITY).

    /// Mount each enabled plugin-contributed `Panel` into the existing dock area
    /// (design §6 Phase 2). Adds one [`PluginPanel`](crate::ui::plugins::pluginpanel::PluginPanel)
    /// tab per `UiContribution::Panel`, docked on the contribution's
    /// [`DockSide`](crate::plugin::DockSide) (the demo's is `Right`, so it tabs in
    /// beside Modules/Bookmarks), and subscribes to its
    /// [`PluginPanelEvent`](crate::ui::plugins::pluginpanel::PluginPanelEvent) so a widget event
    /// routes through the manager. The target dock is **not** forced open, so a
    /// closed dock stays closed (no launch-time behavior change). Empty list ⇒
    /// no-op (parity).
    pub(super) fn mount_plugin_panels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::ui::plugins::pluginpanel::PluginPanel;
        // Collect the panel contributions first (ends the borrow on the manager
        // before we mount gpui views / subscribe to `self`).
        let panels: Vec<(
            String,
            String,
            crate::plugin::DockSide,
            crate::plugin::ViewTree,
        )> = self
            .plugin_manager
            .ui_contributions()
            .into_iter()
            .filter_map(|c| match c {
                crate::plugin::UiContribution::Panel {
                    id,
                    title,
                    dock,
                    initial,
                } => Some((id, title, dock, initial)),
                _ => None,
            })
            .collect();
        for (id, title, dock, initial) in panels {
            let panel = PluginPanel::view(id.clone(), title, initial, window, cx);
            // Route the panel's events through the manager + a scoped live host.
            let sub = cx.subscribe_in(
                &panel,
                window,
                |this,
                 panel,
                 ev: &crate::ui::plugins::pluginpanel::PluginPanelEvent,
                 window,
                 cx| {
                    this.route_plugin_panel_event(panel, ev, window, cx);
                },
            );
            self.plugin_panel_subs.push(sub);
            // Tab into the existing dock at the contributed side (does not open it).
            let placement = dock_placement_for(dock);
            let panel_view: std::sync::Arc<dyn gpui_component::dock::PanelView> =
                std::sync::Arc::new(panel.clone());
            self.dock_area.update(cx, |area, cx| {
                area.add_panel(panel_view, placement, None, window, cx);
            });
            self.plugin_panels.push((id, panel));
        }
    }

    /// A scoped [`LivePluginHost`] wired to the document area + settings — the
    /// shared 4-arg construction behind every F3 plugin-routing path.
    pub(super) fn live_host<'a>(
        &self,
        window: &'a mut Window,
        cx: &'a mut App,
    ) -> crate::ui::plugins::pluginhost::LivePluginHost<'a> {
        crate::ui::plugins::pluginhost::LivePluginHost::new(
            self.document_area.clone(),
            self.settings.clone(),
            window,
            cx,
        )
    }

    /// Route one [`PluginPanelEvent`](crate::ui::plugins::pluginpanel::PluginPanelEvent) through
    /// the manager (design §6 Phase 2 Elm loop): build a scoped `LivePluginHost`,
    /// call `handle_ui_event`, push any fresh tree back into the panel, then drain
    /// the host's collected requests (toasts → `notify`, open-dialogs →
    /// `open_plugin_dialog`, re-renders → `rerender_plugin_panel`).
    pub(super) fn route_plugin_panel_event(
        &mut self,
        panel: &Entity<crate::ui::plugins::pluginpanel::PluginPanel>,
        ev: &crate::ui::plugins::pluginpanel::PluginPanelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Scope the host so its `cx` borrow ends before we re-borrow `cx`.
        let (tree, toasts, open_dialogs, rerenders) = {
            let mut host = self.live_host(window, cx);
            let tree =
                self.plugin_manager
                    .handle_ui_event(&ev.view_id, ev.event.clone(), &mut host);
            let r = host.requests();
            (
                tree,
                r.take_toasts(),
                r.take_open_dialogs(),
                r.take_rerenders(),
            )
        };
        if let Some(tree) = tree {
            panel.update(cx, |p, cx| p.set_tree(tree, window, cx));
        }
        self.drain_plugin_requests(toasts, open_dialogs, rerenders, window, cx);
    }

    /// Re-pull a mounted panel's current `ViewTree` from its owning plugin and push
    /// it into the panel (the [`PluginHost::request_rerender`] resolution, design §3
    /// Elm loop). No-op if `view` isn't a mounted panel or the plugin yields no tree.
    pub(super) fn rerender_plugin_panel(
        &mut self,
        view: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tree) = self.plugin_manager.view_tree(view) else {
            return;
        };
        if let Some((_, panel)) = self.plugin_panels.iter().find(|(id, _)| id == view) {
            let panel = panel.clone();
            panel.update(cx, |p, cx| p.set_tree(tree, window, cx));
        }
    }

    /// Dispatch a plugin-owned command id through the manager + a scoped live host
    /// (design §6 Phase 2), draining the host's collected requests. Reached from
    /// [`run_menu_command`](Self::run_menu_command) for an id
    /// [`is_plugin_command`](crate::plugin::PluginManager::is_plugin_command)
    /// recognizes (a contributed menu/palette item).
    pub(super) fn dispatch_plugin_command(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (cmd_toast, toasts, open_dialogs, rerenders) = {
            let mut host = self.live_host(window, cx);
            let res = self
                .plugin_manager
                .handle_command(id, serde_json::Value::Null, &mut host);
            let r = host.requests();
            let toasts = r.take_toasts();
            // Surface a command's own `CommandResult::toast` ONLY if the handler
            // didn't already push the same message through `host.show_toast` (the
            // demo's ping does both — collecting both here would double-toast).
            let cmd_toast = surface_command_toast(res.toast, &toasts);
            (cmd_toast, toasts, r.take_open_dialogs(), r.take_rerenders())
        };
        if let Some(msg) = cmd_toast {
            self.notify(msg, window, cx);
        }
        self.drain_plugin_requests(toasts, open_dialogs, rerenders, window, cx);
    }

    /// Open a plugin-contributed `Dialog` modally (design §6 Phase 2 — the
    /// generalized C++ `selectTarget`), modeled on
    /// [`open_process_picker`](Self::open_process_picker). Pulls the initial tree +
    /// title from the manager, mounts a [`PluginDialog`](crate::ui::plugins::plugindialog::PluginDialog)
    /// via the proven `window.open_dialog` pattern, and subscribes (on the shared
    /// close-only [`goto_sub`](Self::goto_sub)) to route the dialog's Ui / Closed
    /// events through the manager. No-op if `id` isn't a contributed dialog.
    pub(super) fn open_plugin_dialog(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(initial) = self.plugin_manager.view_tree(id) else {
            return;
        };
        // The contributed title (fall back to the id if somehow absent).
        let title = self
            .plugin_manager
            .ui_contributions()
            .into_iter()
            .find_map(|c| match c {
                crate::plugin::UiContribution::Dialog { id: did, title, .. } if did == id => {
                    Some(title)
                }
                _ => None,
            })
            .unwrap_or_else(|| id.to_string());

        let dialog =
            crate::ui::plugins::plugindialog::PluginDialog::view(id, title, initial, window, cx);
        self.goto_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            |this, dialog, ev: &crate::ui::plugins::plugindialog::PluginDialogEvent, window, cx| {
                match ev {
                    crate::ui::plugins::plugindialog::PluginDialogEvent::Ui { view_id, event } => {
                        let dialog_id = view_id.clone();
                        let (tree, toasts, open_dialogs, close_dialogs, rerenders) = {
                            let mut host = this.live_host(window, cx);
                            let tree = this.plugin_manager.handle_ui_event(
                                view_id,
                                event.clone(),
                                &mut host,
                            );
                            let r = host.requests();
                            (
                                tree,
                                r.take_toasts(),
                                r.take_open_dialogs(),
                                r.take_close_dialogs(),
                                r.take_rerenders(),
                            )
                        };
                        if let Some(tree) = tree {
                            dialog.update(cx, |d, cx| d.set_tree(tree, window, cx));
                        }
                        this.drain_plugin_requests(toasts, open_dialogs, rerenders, window, cx);
                        // If the plugin asked to close THIS dialog (the demo's Attach),
                        // dismiss the modal.
                        if close_dialogs.iter().any(|id| *id == dialog_id) {
                            window.close_dialog(cx);
                        }
                    }
                    crate::ui::plugins::plugindialog::PluginDialogEvent::Closed {
                        view_id,
                        result,
                    } => {
                        let (cmd_toast, toasts, open_dialogs, rerenders) = {
                            let mut host = this.live_host(window, cx);
                            let res = this.plugin_manager.handle_dialog_closed(
                                view_id,
                                result.clone(),
                                &mut host,
                            );
                            let r = host.requests();
                            let toasts = r.take_toasts();
                            // Surface the plugin's `CommandResult::toast` RETURN (the
                            // footer-Submit path — the demo's submit returns "Attached
                            // to …" rather than calling `host.show_toast`), unless the
                            // handler already pushed the same message through the host
                            // (mirror of `dispatch_plugin_command`'s dedup → no
                            // double-toast).
                            let cmd_toast = surface_command_toast(res.toast, &toasts);
                            (cmd_toast, toasts, r.take_open_dialogs(), r.take_rerenders())
                        };
                        if let Some(msg) = cmd_toast {
                            this.notify(msg, window, cx);
                        }
                        this.drain_plugin_requests(toasts, open_dialogs, rerenders, window, cx);
                        window.close_dialog(cx);
                    }
                }
            },
        ));
        self.present_modal(&dialog, 560., 80., None, window, cx);
    }

    /// Drain a scoped live host's collected requests into the window: toasts →
    /// [`notify`](Self::notify), open-dialog ids → [`open_plugin_dialog`](Self::open_plugin_dialog),
    /// re-render view ids → [`rerender_plugin_panel`](Self::rerender_plugin_panel).
    /// Shared by every F3 routing path so the drain order is uniform.
    pub(super) fn drain_plugin_requests(
        &mut self,
        toasts: Vec<String>,
        open_dialogs: Vec<String>,
        rerenders: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for msg in toasts {
            self.notify(msg, window, cx);
        }
        for id in open_dialogs {
            self.open_plugin_dialog(&id, window, cx);
        }
        for view in rerenders {
            self.rerender_plugin_panel(&view, window, cx);
        }
    }

    /// Plugins ▸ Manage Plugins… — open the [`PluginManagerDialog`] (the C++
    /// `showPluginsDialog`; main.cpp:8821). The C++ lists each loaded `IPlugin`
    /// (name, version, description, type, author) with Load/Unload buttons backed
    /// by native `dlopen`; this port lists the in-tree providers in every build and
    /// enables runtime plugin loading only when the `plugins` feature is compiled.
    pub(super) fn open_plugins_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Render the dialog from the SESSION-OWNED manager's live plugin set (design
        // §6 Phase 6 / §7.A [fix]) — not a throwaway `with_builtins()`. An
        // enable/disable here flips THIS manager (the same registry the source
        // pickers read) and persists via its DiskSettings-backed store.
        let dialog = cx.new(|cx| {
            PluginManagerDialog::new(
                plugin_infos_from_rows(self.plugin_manager.plugins_view()),
                cx,
            )
        });
        // Surface the session manager's retained native-load failures (design §7.A
        // [fix]) so ABI/load errors are visible with detail in the dialog instead of
        // only logged at startup. Feature-gated: builds without `plugins` have no
        // loader, so there are no errors to push (and `set_load_errors` doesn't exist).
        #[cfg(feature = "plugins")]
        {
            let errs = self.plugin_manager.load_errors().to_vec();
            if !errs.is_empty() {
                dialog.update(cx, |d, cx| d.set_load_errors(errs, cx));
            }
        }
        let focus = dialog.read(cx).focus_handle(cx);
        // The dialog reports Close + Toggle; Toggle drives the owned manager and
        // pushes the refreshed rows back so the chip reflects the real state.
        self.goto_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            |this, dialog, ev: &PluginManagerEvent, window, cx| match ev {
                PluginManagerEvent::Close => window.close_dialog(cx),
                PluginManagerEvent::Toggle {
                    identifier,
                    enabled,
                } => {
                    // Flip + persist on the session-owned manager (the single source
                    // the pickers read), then re-render the dialog from the refreshed
                    // view so the enabled chip + button label track reality.
                    this.plugin_manager.set_enabled(identifier, *enabled, true);
                    let rows = plugin_infos_from_rows(this.plugin_manager.plugins_view());
                    dialog.update(cx, |d, cx| d.set_plugins(rows, cx));
                }
                PluginManagerEvent::Unload { identifier } => {
                    // Safe-unload through the LIVE host (design §7.A [fix]): build a
                    // window-backed host that detaches affected documents FIRST
                    // (manager.rs safe_unload step 1), drop the plugin + its backing
                    // library, then refresh the rows. The host borrows `cx`, so it is
                    // scoped + dropped before we re-borrow `cx` for `notify`/`update`.
                    let toasts = {
                        let mut host = this.live_host(window, cx);
                        this.plugin_manager.safe_unload(identifier, &mut host);
                        host.take_toasts()
                    };
                    // Drain any toasts the unload path surfaced (the host can't call
                    // `notify` itself — it lacks `&mut MainWindow`).
                    for msg in toasts {
                        this.notify(msg, window, cx);
                    }
                    let rows = plugin_infos_from_rows(this.plugin_manager.plugins_view());
                    dialog.update(cx, |d, cx| d.set_plugins(rows, cx));
                }
                #[cfg(feature = "plugins")]
                PluginManagerEvent::Load => {
                    this.load_plugin_from_path(dialog.clone(), window, cx);
                }
            },
        ));
        self.present_modal(&dialog, 620., 80., Some(&focus), window, cx);
    }

    /// Load a native plugin from a user-chosen path (the C++ load-from-path;
    /// design §6 Phase 3/6) — only compiled behind the `plugins` feature. Pops the
    /// native path picker; on a chosen library, loads it through the session-owned
    /// [`PluginManager`](crate::plugin::PluginManager) (the same registry the
    /// source pickers read), surfaces a load / ABI-mismatch error via a toast
    /// (design §7.A [fix]), and refreshes the open dialog's rows so the new plugin
    /// appears. The chosen-path filter keeps only a real library extension
    /// (`.so`/`.dll`/`.dylib`) so a stray pick is rejected cleanly.
    #[cfg(feature = "plugins")]
    pub(super) fn load_plugin_from_path(
        &mut self,
        dialog: Entity<PluginManagerDialog>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Load a native plugin (.so/.dll/.dylib)".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = rx
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten()
                .and_then(|v| v.into_iter().next())
            else {
                return;
            };
            // Only accept a real shared-library extension (reject a stray pick).
            let ok_ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| matches!(e.to_ascii_lowercase().as_str(), "so" | "dll" | "dylib"))
                .unwrap_or(false);
            let _ = this.update_in(cx, |me, window, cx| {
                if !ok_ext {
                    me.notify(
                        format!("Not a plugin library: {}", path.display()),
                        window,
                        cx,
                    );
                    return;
                }
                match me.plugin_manager.load_native_plugin(&path) {
                    Ok(id) => {
                        me.notify(format!("Loaded plugin '{id}'"), window, cx);
                        // Refresh the open dialog's rows so the new plugin appears.
                        let rows = plugin_infos_from_rows(me.plugin_manager.plugins_view());
                        dialog.update(cx, |d, cx| d.set_plugins(rows, cx));
                    }
                    Err(e) => {
                        // Surface the load / ABI-mismatch error with detail (design
                        // §7.A [fix] — C++ shows a generic "check the console" box).
                        me.notify(format!("Plugin load failed: {e}"), window, cx);
                    }
                }
            });
        })
        .detach();
    }
}
