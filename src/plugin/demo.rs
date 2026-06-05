//! The in-tree **demo plugin** — the Phase-2 deliverable (design §6 Phase 2:
//! "an in-tree demo plugin adds a menu item, a panel, and a dialog").
//!
//! [`DemoPlugin`] is the in-tree analogue that proves the whole Phase-2 surface
//! end-to-end *without a loader and without gpui*: it contributes a `Command`
//! (menu), a `Panel` (a dock view rendered from a [`ViewTree`]), and a `Dialog`
//! (the generalized C++ `selectTarget` process-picker shape — a filter input
//! over a table with an Attach button). Its [`Plugin`] impl drives the Elm loop
//! the Phase-2 declarative host renders:
//!
//! - [`Plugin::handle_command`] — `demo.ping` shows a toast; `demo.open_target`
//!   opens the dialog via [`PluginHost::open_dialog`]; `demo.refresh` bumps a
//!   counter and asks the host to re-render the panel via
//!   [`PluginHost::request_rerender`], returning the fresh panel
//!   [`ViewTree`](crate::plugin::view::ViewTree).
//! - [`Plugin::handle_ui_event`] — the panel's Refresh `Button` and the dialog's
//!   Attach `Button` (the Elm update): Attach calls
//!   [`PluginHost::set_data_source`] + [`PluginHost::close_dialog`] and rebuilds
//!   the dialog tree; the panel Refresh rebuilds the panel tree.
//! - [`Plugin::handle_dialog_closed`] — when the host reports a submitted dialog
//!   (the generalized `selectTarget` outcome), set the data source from the
//!   chosen target.
//!
//! This is **pure data + logic** (no gpui): the host renders the trees
//! ([`crate::ui::plugins::pluginview`]) and routes the events back. The `#[cfg(test)]`
//! suite drives the plugin entirely through [`MockPluginHost`].

use serde_json::Value;

use crate::plugin::contract::{CommandResult, CommandSlot, Contribution, DialogResult, Plugin};
use crate::plugin::host::PluginHost;
use crate::plugin::manifest::{Permission, PluginManifest};
use crate::plugin::view::{UiEvent, ViewTree};

/// Command id: show a toast (the simplest menu contribution).
pub const CMD_PING: &str = "demo.ping";
/// Command id: open the target-picker `Dialog` (the generalized `selectTarget`).
pub const CMD_OPEN_TARGET: &str = "demo.open_target";
/// Command id / panel button id: refresh the panel (bump the tick counter).
pub const CMD_REFRESH: &str = "demo.refresh";
/// The contributed `Panel` id.
pub const PANEL_ID: &str = "demo.panel";
/// The contributed `Dialog` id (the target picker).
pub const DIALOG_ID: &str = "demo.target";
/// The dialog's target `TextInput` id (also the field the host returns on submit).
pub const FIELD_TARGET: &str = "target";
/// The dialog's process `Table` id.
pub const TABLE_PROCS: &str = "demo.target.procs";
/// The dialog's Attach `Button` id.
pub const BTN_ATTACH: &str = "demo.target.attach";
/// The provider identifier the demo plugin attaches as (its derived identifier;
/// `derive_identifier("Plugin Demo")`).
pub const DEMO_IDENTIFIER: &str = "plugindemo";

/// The in-tree demo plugin (design §6 Phase 2). Holds the small bit of state the
/// Elm loop mutates: the panel refresh counter and the dialog's pending target.
pub struct DemoPlugin {
    manifest: PluginManifest,
    /// How many times the panel has been refreshed (rendered in the panel's
    /// KeyValue — proves `request_rerender` round-trips a fresh tree).
    refresh_ticks: u32,
    /// The target string the dialog's Attach button last committed (rendered in
    /// the panel after a successful attach).
    last_target: Option<String>,
    /// The candidate rows the dialog's process table shows (the generalized
    /// `selectTarget` process list; static here since live enumeration is out of
    /// scope, mirroring [`crate::ui::pickers::processpicker`]).
    candidates: Vec<(u32, String)>,
}

impl Default for DemoPlugin {
    fn default() -> Self {
        DemoPlugin::new()
    }
}

impl DemoPlugin {
    /// Construct the demo plugin with its manifest + a small static candidate set.
    pub fn new() -> Self {
        DemoPlugin {
            manifest: PluginManifest::builtin(
                "Plugin Demo",
                "In-tree demo: a menu command, a dock panel, and a target dialog \
                 (the Phase-2 declarative-UI deliverable).",
                vec![Permission::AddUi],
            ),
            refresh_ticks: 0,
            last_target: None,
            candidates: vec![
                (1234, "notepad.exe".to_string()),
                (5678, "game.exe".to_string()),
                (9012, "explorer.exe".to_string()),
            ],
        }
    }

    /// Box the demo plugin (the shape [`PluginManager::add_plugin`] takes).
    pub fn boxed() -> Box<dyn Plugin> {
        Box::new(DemoPlugin::new())
    }

    /// How many times the panel has been refreshed (test hook).
    pub fn refresh_ticks(&self) -> u32 {
        self.refresh_ticks
    }

    /// The last attached target, if any (test hook).
    pub fn last_target(&self) -> Option<&str> {
        self.last_target.as_deref()
    }

    /// Build the panel's current [`ViewTree`] (design §3: Column[Label, KeyValue,
    /// Button]). Pure — the host renders it; re-asked on every refresh so the
    /// counter / last-target reflect the live state.
    pub fn panel_tree(&self) -> ViewTree {
        let target = self
            .last_target
            .clone()
            .unwrap_or_else(|| "(none)".to_string());
        ViewTree::Column(vec![
            ViewTree::Label("Plugin Demo".to_string()),
            ViewTree::KeyValue(vec![
                ("Refreshes".to_string(), self.refresh_ticks.to_string()),
                ("Attached target".to_string(), target),
            ]),
            ViewTree::Button {
                id: CMD_REFRESH.to_string(),
                label: "Refresh".to_string(),
            },
        ])
    }

    /// Build the dialog's current [`ViewTree`] (the generalized `selectTarget`
    /// shape, design §3: a Column with a target `TextInput` + a process `Table`
    /// + an Attach `Button`). Pure.
    pub fn dialog_tree(&self) -> ViewTree {
        let rows: Vec<Vec<String>> = self
            .candidates
            .iter()
            .map(|(pid, name)| vec![pid.to_string(), name.clone()])
            .collect();
        ViewTree::Column(vec![
            ViewTree::Label("Select a target to attach".to_string()),
            ViewTree::TextInput {
                id: FIELD_TARGET.to_string(),
                value: String::new(),
                placeholder: "<pid>:<name>, or pick below".to_string(),
            },
            ViewTree::Table {
                id: TABLE_PROCS.to_string(),
                columns: vec!["PID".to_string(), "Process".to_string()],
                rows,
            },
            ViewTree::Separator,
            ViewTree::Button {
                id: BTN_ATTACH.to_string(),
                label: "Attach".to_string(),
            },
        ])
    }

    /// Map a process `Table` row index to a `"<pid>:<name>"` target string (the
    /// C++ `selectTarget` return format).
    fn target_for_row(&self, row: usize) -> Option<String> {
        self.candidates
            .get(row)
            .map(|(pid, name)| format!("{pid}:{name}"))
    }

    /// Commit `target` as the active data source: record it, set the host data
    /// source, and close the dialog (the Attach action's effect).
    fn attach(&mut self, target: &str, host: &mut dyn PluginHost) {
        self.last_target = Some(target.to_string());
        host.set_data_source(DEMO_IDENTIFIER, target);
        host.close_dialog(DIALOG_ID);
        host.show_toast(&format!("Attached to {target}"));
    }
}

impl Plugin for DemoPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn contributions(&self) -> Vec<Contribution> {
        vec![
            Contribution::Command {
                id: CMD_PING.to_string(),
                title: "Demo: Ping".to_string(),
                slot: CommandSlot::Menu,
            },
            Contribution::Command {
                id: CMD_OPEN_TARGET.to_string(),
                title: "Demo: Select Target\u{2026}".to_string(),
                slot: CommandSlot::Menu,
            },
            Contribution::Panel {
                id: PANEL_ID.to_string(),
                title: "Plugin Demo".to_string(),
                dock: crate::plugin::contract::DockSide::Right,
                initial: self.panel_tree(),
            },
            Contribution::Dialog {
                id: DIALOG_ID.to_string(),
                title: "Select Target".to_string(),
                initial: self.dialog_tree(),
            },
        ]
    }

    fn handle_command(
        &mut self,
        id: &str,
        _args: Value,
        host: &mut dyn PluginHost,
    ) -> CommandResult {
        match id {
            CMD_PING => {
                host.show_toast("Plugin Demo: pong");
                CommandResult::toast("Plugin Demo: pong")
            }
            CMD_OPEN_TARGET => {
                host.open_dialog(DIALOG_ID);
                CommandResult::handled()
            }
            CMD_REFRESH => {
                self.refresh_ticks += 1;
                // Ask the host to push the fresh panel tree into the open panel
                // (the Elm loop driven from a command, not a UI event).
                host.request_rerender(PANEL_ID);
                CommandResult::handled()
            }
            _ => CommandResult::default(),
        }
    }

    fn handle_ui_event(
        &mut self,
        view: &str,
        ev: UiEvent,
        host: &mut dyn PluginHost,
    ) -> Option<ViewTree> {
        match (view, ev) {
            // Panel Refresh button — bump the counter, rebuild the panel.
            (PANEL_ID, UiEvent::Clicked(id)) if id == CMD_REFRESH => {
                self.refresh_ticks += 1;
                Some(self.panel_tree())
            }
            // Dialog Attach button — commit the first candidate (no row picked)
            // or whatever the table/text last selected; rebuild the dialog tree.
            (DIALOG_ID, UiEvent::Clicked(id)) if id == BTN_ATTACH => {
                if let Some(target) = self.target_for_row(0) {
                    self.attach(&target, host);
                }
                Some(self.dialog_tree())
            }
            // Dialog row selection — typing the chosen target into the field
            // (the host then submits it). Rebuild with the row reflected.
            (DIALOG_ID, UiEvent::RowSelected { table, row }) if table == TABLE_PROCS => {
                if let Some(target) = self.target_for_row(row) {
                    self.attach(&target, host);
                }
                Some(self.dialog_tree())
            }
            // Dialog target field submitted (Enter) — attach to the typed value.
            (DIALOG_ID, UiEvent::Submitted { id, text }) if id == FIELD_TARGET => {
                if !text.is_empty() {
                    self.attach(&text, host);
                }
                Some(self.dialog_tree())
            }
            _ => None,
        }
    }

    fn handle_dialog_closed(
        &mut self,
        view: &str,
        result: DialogResult,
        host: &mut dyn PluginHost,
    ) -> CommandResult {
        if view != DIALOG_ID {
            return CommandResult::default();
        }
        match result {
            DialogResult::Submitted { .. } => {
                if let Some(target) = result.get(FIELD_TARGET) {
                    // A host that gathered the field values itself: commit them.
                    self.last_target = Some(target.to_string());
                    host.set_data_source(DEMO_IDENTIFIER, target);
                    return CommandResult::toast(format!("Attached to {target}"));
                }
                CommandResult::handled()
            }
            DialogResult::Cancelled => CommandResult::handled(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::host::MockPluginHost;

    #[test]
    fn contributions_have_one_command_panel_dialog_each() {
        let p = DemoPlugin::new();
        let contribs = p.contributions();
        let commands = contribs
            .iter()
            .filter(|c| matches!(c, Contribution::Command { .. }))
            .count();
        let panels = contribs
            .iter()
            .filter(|c| matches!(c, Contribution::Panel { .. }))
            .count();
        let dialogs = contribs
            .iter()
            .filter(|c| matches!(c, Contribution::Dialog { .. }))
            .count();
        // Two commands (ping + open-target), one panel, one dialog.
        assert_eq!(commands, 2);
        assert_eq!(panels, 1);
        assert_eq!(dialogs, 1);
        // The renderer enumerates exactly the panel + dialog as views.
        assert_eq!(Contribution::view_ids(&contribs), [PANEL_ID, DIALOG_ID]);
    }

    #[test]
    fn ping_shows_a_toast() {
        let mut p = DemoPlugin::new();
        let mut host = MockPluginHost::new();
        let res = p.handle_command(CMD_PING, Value::Null, &mut host);
        assert!(res.handled);
        assert_eq!(res.toast.as_deref(), Some("Plugin Demo: pong"));
        assert_eq!(host.toasts(), ["Plugin Demo: pong"]);
    }

    #[test]
    fn open_target_opens_the_dialog() {
        let mut p = DemoPlugin::new();
        let mut host = MockPluginHost::new();
        let res = p.handle_command(CMD_OPEN_TARGET, Value::Null, &mut host);
        assert!(res.handled);
        assert_eq!(host.opened_dialogs(), [DIALOG_ID]);
    }

    #[test]
    fn refresh_command_bumps_counter_and_requests_rerender() {
        let mut p = DemoPlugin::new();
        let mut host = MockPluginHost::new();
        assert_eq!(p.refresh_ticks(), 0);
        let res = p.handle_command(CMD_REFRESH, Value::Null, &mut host);
        assert!(res.handled);
        assert_eq!(p.refresh_ticks(), 1);
        assert_eq!(host.rerender_requests(), [PANEL_ID]);
    }

    #[test]
    fn panel_refresh_button_returns_fresh_tree() {
        let mut p = DemoPlugin::new();
        let mut host = MockPluginHost::new();
        let tree = p.handle_ui_event(
            PANEL_ID,
            UiEvent::Clicked(CMD_REFRESH.to_string()),
            &mut host,
        );
        // Some(tree) re-renders the panel; the counter advanced.
        let tree = tree.expect("a fresh panel tree");
        assert_eq!(p.refresh_ticks(), 1);
        // The fresh tree reports the new counter in its KeyValue.
        let ViewTree::Column(children) = &tree else {
            panic!("panel root is a Column");
        };
        let has_count = children.iter().any(|c| {
            matches!(c, ViewTree::KeyValue(pairs)
                if pairs.iter().any(|(k, v)| k == "Refreshes" && v == "1"))
        });
        assert!(has_count, "panel KeyValue reflects the bumped counter");
    }

    #[test]
    fn dialog_attach_sets_data_source_and_closes() {
        let mut p = DemoPlugin::new();
        let mut host = MockPluginHost::new();
        let tree = p.handle_ui_event(
            DIALOG_ID,
            UiEvent::Clicked(BTN_ATTACH.to_string()),
            &mut host,
        );
        assert!(tree.is_some(), "Attach re-renders the dialog");
        // The Attach button committed the first candidate as the data source,
        // closed the dialog, and toasted.
        assert_eq!(
            host.data_source(),
            Some(&(DEMO_IDENTIFIER.to_string(), "1234:notepad.exe".to_string()))
        );
        assert_eq!(host.closed_dialogs(), [DIALOG_ID]);
        assert_eq!(p.last_target(), Some("1234:notepad.exe"));
    }

    #[test]
    fn dialog_row_selection_attaches_that_row() {
        let mut p = DemoPlugin::new();
        let mut host = MockPluginHost::new();
        p.handle_ui_event(
            DIALOG_ID,
            UiEvent::RowSelected {
                table: TABLE_PROCS.to_string(),
                row: 1,
            },
            &mut host,
        );
        // Row 1 is game.exe / 5678.
        assert_eq!(p.last_target(), Some("5678:game.exe"));
        assert_eq!(
            host.data_source(),
            Some(&(DEMO_IDENTIFIER.to_string(), "5678:game.exe".to_string()))
        );
    }

    #[test]
    fn dialog_text_submit_attaches_typed_target() {
        let mut p = DemoPlugin::new();
        let mut host = MockPluginHost::new();
        p.handle_ui_event(
            DIALOG_ID,
            UiEvent::Submitted {
                id: FIELD_TARGET.to_string(),
                text: "4242:custom.exe".to_string(),
            },
            &mut host,
        );
        assert_eq!(p.last_target(), Some("4242:custom.exe"));
    }

    #[test]
    fn handle_dialog_closed_submitted_sets_source() {
        let mut p = DemoPlugin::new();
        let mut host = MockPluginHost::new();
        let res = p.handle_dialog_closed(
            DIALOG_ID,
            DialogResult::Submitted {
                values: vec![(FIELD_TARGET.to_string(), "1:host.exe".to_string())],
            },
            &mut host,
        );
        assert!(res.handled);
        assert_eq!(p.last_target(), Some("1:host.exe"));
        assert_eq!(
            host.data_source(),
            Some(&(DEMO_IDENTIFIER.to_string(), "1:host.exe".to_string()))
        );
    }

    #[test]
    fn handle_dialog_closed_cancelled_is_inert() {
        let mut p = DemoPlugin::new();
        let mut host = MockPluginHost::new();
        let res = p.handle_dialog_closed(DIALOG_ID, DialogResult::Cancelled, &mut host);
        assert!(res.handled);
        assert!(p.last_target().is_none());
        assert!(host.data_source().is_none());
    }

    #[test]
    fn unrelated_view_event_is_ignored() {
        let mut p = DemoPlugin::new();
        let mut host = MockPluginHost::new();
        // An event for a view/id the demo doesn't own returns None (no re-render).
        assert_eq!(
            p.handle_ui_event("other", UiEvent::Clicked("x".to_string()), &mut host),
            None
        );
        assert!(host.data_source().is_none());
    }

    #[test]
    fn identifier_matches_derive_identifier() {
        // The provider identifier constant matches the centralized derivation of
        // the manifest name, so wiring can't drift.
        let p = DemoPlugin::new();
        assert_eq!(p.manifest().identifier(), DEMO_IDENTIFIER);
    }
}
