//! `example-ui` — a minimal native Reclass **UI** plugin (design §6 Phase 3
//! deliverable, §G example). It contributes a `Command`, a dock `Panel`, and a
//! modal `Dialog` (the generalized C++ `selectTarget`) as declarative `ViewTree`s
//! the host renders. No provider — this proves the "add UI" goal crosses the ABI
//! with only plain data (design §3).

use reclass_plugin::{
    CommandResult, CommandSlot, Contribution, DialogResult, DockSide, Host, Manifest, Permission,
    Plugin, ViewTree,
};

const CMD_PING: &str = "example.ui.ping";
const PANEL_ID: &str = "example.ui.panel";
const DIALOG_ID: &str = "example.ui.dialog";
const BTN_OPEN: &str = "example.ui.open";
const FIELD_TARGET: &str = "example.ui.target";

#[derive(Default)]
struct ExampleUiPlugin {
    clicks: u32,
}

impl ExampleUiPlugin {
    fn panel_tree(&self) -> ViewTree {
        ViewTree::Column(vec![
            ViewTree::Label(format!("Example UI panel — opens: {}", self.clicks)),
            ViewTree::Separator,
            ViewTree::Button {
                id: BTN_OPEN.to_string(),
                label: "Open target picker".to_string(),
            },
        ])
    }
}

impl Plugin for ExampleUiPlugin {
    fn manifest(&self) -> Manifest {
        Manifest::new("Example UI", env!("CARGO_PKG_VERSION"))
            .author("Reclass (Rust port) examples")
            .description("A panel + dialog demo, loaded as a native plugin.")
            .permissions(vec![Permission::AddUi])
    }

    fn contributions(&self) -> Vec<Contribution> {
        vec![
            Contribution::Command {
                id: CMD_PING.to_string(),
                title: "Example: Ping".to_string(),
                slot: CommandSlot::Palette,
            },
            Contribution::Panel {
                id: PANEL_ID.to_string(),
                title: "Example UI".to_string(),
                dock: DockSide::Right,
                initial: self.panel_tree(),
            },
            Contribution::Dialog {
                id: DIALOG_ID.to_string(),
                title: "Pick a target".to_string(),
                initial: ViewTree::Column(vec![
                    ViewTree::Label("Enter a target string:".to_string()),
                    ViewTree::TextInput {
                        id: FIELD_TARGET.to_string(),
                        value: String::new(),
                        placeholder: "1234:demo.exe".to_string(),
                    },
                ]),
            },
        ]
    }

    fn handle_command(&mut self, id: &str, _args: &str, host: &mut Host<'_>) -> CommandResult {
        if id == CMD_PING {
            host.show_toast("Example UI: pong");
            CommandResult::handled()
        } else {
            CommandResult::default()
        }
    }

    fn handle_ui_event(
        &mut self,
        view: &str,
        ev: reclass_plugin::UiEvent,
        host: &mut Host<'_>,
    ) -> Option<ViewTree> {
        use reclass_plugin::UiEvent;
        if view == PANEL_ID {
            if let UiEvent::Clicked(btn) = ev {
                if btn == BTN_OPEN {
                    self.clicks += 1;
                    host.open_dialog(DIALOG_ID);
                    return Some(self.panel_tree());
                }
            }
        }
        None
    }

    fn handle_dialog_closed(
        &mut self,
        view: &str,
        result: DialogResult,
        host: &mut Host<'_>,
    ) -> CommandResult {
        if view == DIALOG_ID {
            if let DialogResult::Submitted { values } = result {
                if let Some((_, target)) = values.into_iter().find(|(k, _)| k == FIELD_TARGET) {
                    host.show_toast(format!("Example UI: picked {target}"));
                    return CommandResult::handled();
                }
            }
        }
        CommandResult::default()
    }
}

reclass_plugin::export_plugin! {
    new_plugin: || ExampleUiPlugin::default(),
    // UI-only plugin: no provider. can_handle always false; create_provider errors.
    can_handle: |_target: &str| false,
    create_provider: |_target: &str| -> Result<reclass_plugin::NoProvider, String> {
        Err("example-ui contributes no provider".to_string())
    },
    enumerate_processes: || Vec::new(),
}
