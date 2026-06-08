//! memflow attach dialog: choose a connector/OS chain, connect, then select a
//! process from the same process-table model used by the other process sources.

use std::io::ErrorKind;
use std::process::Command;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::table::{DataTable, TableEvent, TableState};
use gpui_component::{ActiveTheme as _, Disableable as _, Sizable as _};

use crate::plugin::contract::ProcessInfo;
use crate::provider::memflow::{
    default_plugin_dir, enumerate_processes, inventory_info, MemflowAttachConfig,
    MemflowInventoryInfo,
};
use crate::ui::design::{color, tokens};
use crate::ui::dialogs::modal;
use crate::ui::pickers::processpicker::{
    preferred_row_index, ProcessPickerModel, ProcessRow, ProcessTableDelegate,
};

const MEMFLOW_PROVIDER_ID: &str = "memflowprocessmemory";
const MEMFLOW_DIALOG_WIDTH: f32 = 720.;
const ACTIVITY_LIMIT: usize = 80;

#[derive(Clone, Copy)]
struct ConnectorPreset {
    name: &'static str,
    os: &'static str,
    connector_args: &'static str,
    os_args: &'static str,
    connector_placeholder: &'static str,
    os_placeholder: &'static str,
}

const CONNECTOR_PRESETS: &[ConnectorPreset] = &[
    ConnectorPreset {
        name: "qemu",
        os: "win32",
        connector_args: "",
        os_args: "",
        connector_placeholder: "VM name or qmp=...",
        os_placeholder: ":dtb=0x...",
    },
    ConnectorPreset {
        name: "kvm",
        os: "win32",
        connector_args: "",
        os_args: "",
        connector_placeholder: "VM name / domain",
        os_placeholder: ":dtb=0x...",
    },
    ConnectorPreset {
        name: "pcileech",
        os: "win32",
        connector_args: "device=FPGA",
        os_args: "",
        connector_placeholder: "device=FPGA",
        os_placeholder: ":dtb=0x...",
    },
    ConnectorPreset {
        name: "coredump",
        os: "win32",
        connector_args: "",
        os_args: "",
        connector_placeholder: "/path/to/dump.raw",
        os_placeholder: ":dtb=0x...",
    },
    ConnectorPreset {
        name: "winio",
        os: "win32",
        connector_args: "",
        os_args: "",
        connector_placeholder: "driver args",
        os_placeholder: ":dtb=0x...",
    },
];

const STANDARD_MEMFLOW_PLUGINS: &[&str] = &["win32", "qemu", "kvm", "pcileech", "coredump"];
const STANDARD_OS_LAYERS: &[&str] = &["win32"];

#[derive(Clone, Copy)]
enum PluginField {
    Connector,
    Os,
}

#[derive(Clone)]
struct PluginChoice {
    name: String,
    installed: bool,
}

struct CommandStepResult {
    command: String,
    ok: bool,
    missing: bool,
    detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingInstallAfter {
    Connect,
    Stay,
}

#[derive(Clone, Debug)]
struct InstallConfirmation {
    reason: String,
    install_dir: String,
    after: PendingInstallAfter,
}

#[derive(Clone, Debug)]
pub enum MemflowAttachEvent {
    Attach(MemflowAttachConfig),
    Cancel,
}

pub struct MemflowAttachDialog {
    connector: Entity<InputState>,
    connector_args: Entity<InputState>,
    os: Entity<InputState>,
    os_args: Entity<InputState>,
    plugin_dirs: Entity<InputState>,
    process_filter: Entity<InputState>,
    manual_pid: Entity<InputState>,
    manual_process_name: Entity<InputState>,
    writable: bool,
    inventory: MemflowInventoryInfo,
    process_model: ProcessPickerModel,
    process_table: Entity<TableState<ProcessTableDelegate>>,
    selected_process: Option<ProcessInfo>,
    connected: bool,
    installing_plugins: bool,
    pending_install: Option<InstallConfirmation>,
    install_confirmed_once: bool,
    status: String,
    activity: Vec<String>,
    focus_handle: FocusHandle,
    _subs: Vec<Subscription>,
}

impl MemflowAttachDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let plugin_dir_seed = default_plugin_dir().unwrap_or_default();
        let inventory_dirs = if plugin_dir_seed.is_empty() {
            Vec::new()
        } else {
            vec![plugin_dir_seed.clone()]
        };
        let inventory = inventory_info(&inventory_dirs);
        let connector_seed = preferred_connector(&inventory.connectors);
        let os_seed = if inventory.os_layers.iter().any(|s| s == "win32") {
            "win32".to_string()
        } else {
            inventory
                .os_layers
                .first()
                .cloned()
                .unwrap_or_else(|| "win32".to_string())
        };

        let connector = input(cx, window, "qemu / kvm / pcileech / winio", connector_seed);
        let connector_args = input(cx, window, "connector args", String::new());
        let os = input(cx, window, "win32", os_seed);
        let os_args = input(cx, window, "os args", String::new());
        let plugin_dirs = input(
            cx,
            window,
            "Optional extra dirs; default paths are scanned",
            plugin_dir_seed,
        );
        let process_filter = input(cx, window, "Filter by name, PID, or path", String::new());
        let manual_pid = input(cx, window, "pid", String::new());
        let manual_process_name = input(cx, window, "process.exe", String::new());
        let path_w =
            px((f32::from(modal::clamp_width(MEMFLOW_DIALOG_WIDTH, window)) - 390.).max(180.));
        let process_table = cx.new(|cx| {
            TableState::new(ProcessTableDelegate::new(path_w), window, cx).row_selectable(true)
        });

        let mut subs = Vec::new();
        for input in [
            connector.clone(),
            connector_args.clone(),
            os.clone(),
            os_args.clone(),
        ] {
            subs.push(
                cx.subscribe_in(&input, window, |this, _input, ev: &InputEvent, _w, cx| {
                    if matches!(ev, InputEvent::Change) {
                        this.invalidate_connection(cx);
                    }
                }),
            );
        }
        subs.push(cx.subscribe_in(
            &plugin_dirs,
            window,
            |this, _input, ev: &InputEvent, _w, cx| match ev {
                InputEvent::Change => {
                    this.invalidate_connection(cx);
                    this.auto_refresh_inventory(cx);
                }
                InputEvent::Blur | InputEvent::PressEnter { .. } => this.refresh_inventory(cx),
                _ => {}
            },
        ));
        for input in [manual_pid.clone(), manual_process_name.clone()] {
            subs.push(cx.subscribe(&input, |_this, _input, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    cx.notify();
                }
            }));
        }
        subs.push(
            cx.subscribe(&process_filter, |this, _input, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.refresh_process_table(cx);
                }
            }),
        );
        subs.push(cx.subscribe(
            &process_table,
            |this, table, ev: &TableEvent, cx| match ev {
                TableEvent::SelectRow(row_ix) => {
                    let row = table.read(cx).delegate().rows().get(*row_ix).cloned();
                    if let Some(row) = row {
                        this.select_process_row(&row, cx);
                    }
                }
                TableEvent::DoubleClickedRow(row_ix) => {
                    let row = table.read(cx).delegate().rows().get(*row_ix).cloned();
                    if let Some(row) = row {
                        this.select_process_row(&row, cx);
                        this.attach(cx);
                    }
                }
                _ => {}
            },
        ));

        let inventory_message = inventory_status("Inventory scanned", &inventory);

        Self {
            connector,
            connector_args,
            os,
            os_args,
            plugin_dirs,
            process_filter,
            manual_pid,
            manual_process_name,
            writable: false,
            inventory,
            process_model: ProcessPickerModel::default(),
            process_table,
            selected_process: None,
            connected: false,
            installing_plugins: false,
            pending_install: None,
            install_confirmed_once: false,
            status: inventory_message.clone(),
            activity: vec![inventory_message],
            focus_handle: cx.focus_handle(),
            _subs: subs,
        }
    }

    pub fn view(window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(window, cx))
    }

    fn text(input: &Entity<InputState>, cx: &App) -> String {
        input.read(cx).value().to_string()
    }

    fn set_text(
        input: &Entity<InputState>,
        text: impl Into<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = text.into();
        input.update(cx, |state, cx| state.set_value(text, window, cx));
    }

    fn set_placeholder(
        input: &Entity<InputState>,
        placeholder: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        input.update(cx, |state, cx| {
            state.set_placeholder(placeholder.into(), window, cx)
        });
    }

    fn plugin_dirs(&self, cx: &App) -> Vec<String> {
        Self::text(&self.plugin_dirs, cx)
            .split([';', '\n'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    fn install_dir_text(&self, cx: &App) -> String {
        default_plugin_dir()
            .or_else(|| self.plugin_dirs(cx).first().cloned())
            .unwrap_or_else(|| "the memflow default plugin directory".to_string())
    }

    fn selected_standard_plugins_missing(&self, cx: &App) -> Vec<String> {
        let connector = Self::text(&self.connector, cx);
        let connector = connector.trim();
        let os = Self::text(&self.os, cx);
        let os = os.trim();
        let mut missing = Vec::new();

        if standard_memflow_plugin(connector)
            && !inventory_has(&self.inventory.connectors, connector)
        {
            missing.push(format!("connector `{connector}`"));
        }
        if standard_memflow_plugin(os) && !inventory_has(&self.inventory.os_layers, os) {
            missing.push(format!("OS `{os}`"));
        }

        missing
    }

    fn config(&self, require_process: bool, cx: &App) -> Result<MemflowAttachConfig, String> {
        let manual_pid_text = Self::text(&self.manual_pid, cx);
        let manual_pid = if manual_pid_text.trim().is_empty() {
            None
        } else {
            Some(
                manual_pid_text
                    .trim()
                    .parse::<u32>()
                    .map_err(|_| "PID must be a decimal u32".to_string())?,
            )
        };
        let manual_name = Self::text(&self.manual_process_name, cx).trim().to_string();
        let selected_pid = self.selected_process.as_ref().map(|p| p.pid);
        let selected_name = self
            .selected_process
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_default();
        let pid = manual_pid.or_else(|| {
            if manual_name.is_empty() {
                selected_pid
            } else {
                None
            }
        });
        let process_name = if manual_name.is_empty() {
            selected_name
        } else {
            manual_name
        };

        let cfg = MemflowAttachConfig {
            connector: Self::text(&self.connector, cx).trim().to_string(),
            connector_args: Self::text(&self.connector_args, cx).trim().to_string(),
            os: Self::text(&self.os, cx).trim().to_string(),
            os_args: Self::text(&self.os_args, cx).trim().to_string(),
            pid,
            process_name,
            writable: self.writable,
            inventory_dirs: self.plugin_dirs(cx),
        };
        if cfg.connector.is_empty() {
            return Err("Connector is required.".to_string());
        }
        if cfg.os.is_empty() {
            return Err("OS plugin is required, usually win32".to_string());
        }
        if require_process {
            cfg.validate()?;
        }
        Ok(cfg)
    }

    fn invalidate_connection(&mut self, cx: &mut Context<Self>) {
        if self.connected || !self.process_model.rows().is_empty() {
            self.status = "Connection settings changed. Connect again.".to_string();
        }
        self.connected = false;
        self.selected_process = None;
        self.process_model = ProcessPickerModel::default();
        self.refresh_process_table(cx);
        cx.notify();
    }

    fn log(&mut self, message: impl Into<String>) {
        self.activity.push(message.into());
        if self.activity.len() > ACTIVITY_LIMIT {
            let remove = self.activity.len() - ACTIVITY_LIMIT;
            self.activity.drain(0..remove);
        }
    }

    fn auto_refresh_inventory(&mut self, cx: &mut Context<Self>) {
        self.refresh_inventory_inner(false, cx);
    }

    fn refresh_inventory(&mut self, cx: &mut Context<Self>) {
        self.refresh_inventory_inner(true, cx);
    }

    fn refresh_inventory_inner(&mut self, log: bool, cx: &mut Context<Self>) {
        let dirs = self.plugin_dirs(cx);
        self.inventory = inventory_info(&dirs);
        self.status = inventory_status("Inventory refreshed", &self.inventory);
        if log {
            self.log(self.status.clone());
            for warning in self.inventory.warnings.clone() {
                self.log(format!("Inventory warning: {warning}"));
            }
        }
        cx.notify();
    }

    fn apply_connector_selection(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        Self::set_text(&self.connector, name.clone(), window, cx);
        if let Some(preset) = connector_preset(&name) {
            Self::set_text(&self.connector_args, preset.connector_args, window, cx);
            Self::set_placeholder(
                &self.connector_args,
                preset.connector_placeholder,
                window,
                cx,
            );
            Self::set_text(&self.os, preset.os, window, cx);
            Self::set_text(&self.os_args, preset.os_args, window, cx);
            Self::set_placeholder(&self.os_args, preset.os_placeholder, window, cx);
            self.status = format!("Selected {name}; OS preset is {}", preset.os);
        } else {
            self.status = format!("Selected custom connector {name}");
        }
        self.invalidate_connection(cx);
        self.log(self.status.clone());
        cx.notify();
    }

    fn apply_os_selection(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        Self::set_text(&self.os, name.clone(), window, cx);
        let placeholder = if name == "win32" {
            ":dtb=0x..."
        } else {
            "os args"
        };
        Self::set_placeholder(&self.os_args, placeholder, window, cx);
        self.invalidate_connection(cx);
        self.status = format!("Selected OS plugin {name}");
        self.log(self.status.clone());
        cx.notify();
    }

    fn request_standard_plugin_install(&mut self, cx: &mut Context<Self>) {
        self.request_install_confirmation(
            PendingInstallAfter::Stay,
            "Install the standard memflow plugins used by the built-in connector presets."
                .to_string(),
            cx,
        );
    }

    fn request_install_confirmation(
        &mut self,
        after: PendingInstallAfter,
        reason: String,
        cx: &mut Context<Self>,
    ) {
        if self.installing_plugins {
            self.status = "Memflow plugin installation is already running.".to_string();
            self.log(self.status.clone());
            cx.notify();
            return;
        }

        if self.install_confirmed_once {
            self.install_standard_plugins_confirmed(after, cx);
            return;
        }

        self.pending_install = Some(InstallConfirmation {
            reason,
            install_dir: self.install_dir_text(cx),
            after,
        });
        self.status = "Confirm memflow plugin installation.".to_string();
        self.log("Memflow plugin install confirmation shown");
        cx.notify();
    }

    fn confirm_install(&mut self, cx: &mut Context<Self>) {
        let Some(confirmation) = self.pending_install.take() else {
            return;
        };
        self.install_confirmed_once = true;
        self.log("Memflow plugin install confirmed");
        self.install_standard_plugins_confirmed(confirmation.after, cx);
    }

    fn cancel_install_confirmation(&mut self, cx: &mut Context<Self>) {
        if self.pending_install.take().is_some() {
            self.status = "Memflow plugin installation cancelled.".to_string();
            self.log(self.status.clone());
            cx.notify();
        }
    }

    fn install_standard_plugins_confirmed(
        &mut self,
        after: PendingInstallAfter,
        cx: &mut Context<Self>,
    ) {
        if self.installing_plugins {
            return;
        }
        self.installing_plugins = true;
        self.status = "Installing standard memflow plugins...".to_string();
        self.pending_install = None;
        self.log(format!(
            "Plugin install target: {}",
            self.install_dir_text(cx)
        ));
        self.log("Checking cargo availability");
        self.log("Starting memflowup install: win32, qemu, kvm, pcileech, coredump");
        cx.notify();

        cx.spawn(async move |this, cx| {
            let cargo = cx
                .background_spawn(async move { command_step("cargo", &["--version"]) })
                .await;
            let cargo_ok = cargo.ok;
            let cargo_missing = cargo.missing;
            let cargo_detail = cargo.detail.clone();
            let cargo_log = command_log_line(&cargo);
            let _ = this.update(cx, |this, cx| {
                this.log(cargo_log);
                cx.notify();
            });
            if !cargo_ok {
                let status = if cargo_missing {
                    "Cargo is required to install memflow plugins, but it was not found on PATH."
                        .to_string()
                } else {
                    format!("Cargo is required, but `cargo --version` failed: {cargo_detail}")
                };
                let _ = this.update(cx, |this, cx| {
                    this.installing_plugins = false;
                    this.status = status;
                    this.log(this.status.clone());
                    cx.notify();
                });
                return;
            }

            let help = cx
                .background_spawn(async move { command_step("memflowup", &["help"]) })
                .await;
            if help.missing {
                let _ = this.update(cx, |this, cx| {
                    this.log("memflowup was not found; installing it with cargo");
                    cx.notify();
                });
                let install = cx
                    .background_spawn(async move {
                        command_step("cargo", &["install", "memflowup", "--force"])
                    })
                    .await;
                let cargo_ok = install.ok;
                let _ = this.update(cx, |this, cx| {
                    this.log(command_log_line(&install));
                    cx.notify();
                });
                if !cargo_ok {
                    let _ = this.update(cx, |this, cx| {
                        this.installing_plugins = false;
                        this.status = "Could not install memflowup. Install it manually and retry."
                            .to_string();
                        this.log(this.status.clone());
                        cx.notify();
                    });
                    return;
                }
            } else {
                let help_ok = help.ok;
                let help_detail = help.detail.clone();
                let _ = this.update(cx, |this, cx| {
                    this.log(command_log_line(&help));
                    cx.notify();
                });
                if !help_ok {
                    let _ = this.update(cx, |this, cx| {
                        this.installing_plugins = false;
                        this.status = format!(
                            "memflowup is installed but did not run successfully: {help_detail}"
                        );
                        this.log(this.status.clone());
                        cx.notify();
                    });
                    return;
                }
            }

            let mut failed_plugins = Vec::new();
            for plugin in STANDARD_MEMFLOW_PLUGINS {
                let _ = this.update(cx, |this, cx| {
                    this.log(format!("Running memflowup pull {plugin}"));
                    cx.notify();
                });
                let plugin = *plugin;
                let result = cx
                    .background_spawn(async move { command_step("memflowup", &["pull", plugin]) })
                    .await;
                if !result.ok {
                    failed_plugins.push(format!("{plugin}: {}", result.detail));
                }
                let _ = this.update(cx, |this, cx| {
                    this.log(command_log_line(&result));
                    cx.notify();
                });
            }

            let install_failed = !failed_plugins.is_empty();
            let failure_count = failed_plugins.len();
            let _ = this.update(cx, |this, cx| {
                this.installing_plugins = false;
                this.refresh_inventory(cx);
                let missing = this.selected_standard_plugins_missing(cx);
                if !install_failed {
                    this.status =
                        inventory_status("Install finished; inventory refreshed", &this.inventory);
                } else {
                    this.status = format!(
                        "Install finished with {} plugin error(s). See activity.",
                        failure_count
                    );
                    for failure in failed_plugins {
                        this.log(format!("Plugin install failed: {failure}"));
                    }
                }
                this.log(this.status.clone());
                if after == PendingInstallAfter::Connect && !install_failed && missing.is_empty() {
                    this.connect(cx);
                    return;
                }
                if after == PendingInstallAfter::Connect && !missing.is_empty() {
                    this.status = format!(
                        "Install finished, but required plugin(s) are still missing: {}.",
                        missing.join(", ")
                    );
                    this.log(this.status.clone());
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        let cfg = match self.config(false, cx) {
            Ok(cfg) => cfg,
            Err(err) => {
                self.connected = false;
                self.status = format!("Connection failed: {err}");
                self.log(self.status.clone());
                cx.notify();
                return;
            }
        };

        let missing = self.selected_standard_plugins_missing(cx);
        if !missing.is_empty() {
            let reason = format!(
                "Connect needs these missing memflow plugin(s): {}.",
                missing.join(", ")
            );
            self.request_install_confirmation(PendingInstallAfter::Connect, reason, cx);
            return;
        }

        self.log(format!(
            "Opening memflow chain: connector={}, os={}",
            cfg.connector, cfg.os
        ));
        match enumerate_processes(&cfg) {
            Ok(processes) => {
                let count = processes.len();
                self.connected = true;
                self.selected_process = None;
                self.process_model =
                    ProcessPickerModel::from_processes(processes, MEMFLOW_PROVIDER_ID);
                self.status = format!("Connected. Found {count} process(es).");
                self.log(self.status.clone());
                self.refresh_process_table(cx);
            }
            Err(err) => {
                self.connected = false;
                self.selected_process = None;
                self.process_model = ProcessPickerModel::default();
                self.status = format!("Connection failed: {err}");
                self.log(self.status.clone());
                self.refresh_process_table(cx);
            }
        }
        cx.notify();
    }

    fn refresh_process_table(&mut self, cx: &mut Context<Self>) {
        let query = Self::text(&self.process_filter, cx);
        let rows: Vec<ProcessRow> = self
            .process_model
            .filtered(&query)
            .into_iter()
            .cloned()
            .collect();
        let selected_pid = self.selected_process.as_ref().map(|p| p.pid);
        let selected_ix = selected_pid
            .and_then(|pid| rows.iter().position(|row| row.pid == pid))
            .or_else(|| preferred_row_index(&rows));
        self.selected_process = selected_ix.and_then(|ix| {
            rows.get(ix).map(|row| ProcessInfo {
                pid: row.pid,
                name: row.name.clone(),
                path: row.path.clone(),
                is_32bit: row.is_32bit,
            })
        });
        self.process_table.update(cx, |state, cx| {
            state.delegate_mut().set_rows(rows);
            if let Some(ix) = selected_ix {
                state.set_selected_row(ix, cx);
                state.scroll_to_row(ix, cx);
            }
            cx.notify();
        });
        cx.notify();
    }

    fn select_process_row(&mut self, row: &ProcessRow, cx: &mut Context<Self>) {
        self.selected_process = Some(ProcessInfo {
            pid: row.pid,
            name: row.name.clone(),
            path: row.path.clone(),
            is_32bit: row.is_32bit,
        });
        self.status = format!("Selected {} (pid {})", row.name, row.pid);
        cx.notify();
    }

    fn attach(&mut self, cx: &mut Context<Self>) {
        if !self.connected {
            self.status = "Connect before attaching.".to_string();
            self.log(self.status.clone());
            cx.notify();
            return;
        }
        match self.config(true, cx) {
            Ok(cfg) => cx.emit(MemflowAttachEvent::Attach(cfg)),
            Err(err) => {
                self.status = err;
                self.log(self.status.clone());
                cx.notify();
            }
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(MemflowAttachEvent::Cancel);
    }

    fn can_attach(&self, cx: &App) -> bool {
        self.connected
            && (self.selected_process.is_some()
                || !Self::text(&self.manual_pid, cx).trim().is_empty()
                || !Self::text(&self.manual_process_name, cx).trim().is_empty())
    }

    fn render_input(
        &self,
        label: &'static str,
        input: &Entity<InputState>,
        cx: &App,
    ) -> AnyElement {
        gpui_component::v_flex()
            .gap(px(tokens::space::XS))
            .child(modal::field_label(label, cx))
            .child(Input::new(input).small().w_full())
            .into_any_element()
    }

    fn render_plugin_dropdown(
        &self,
        label: &'static str,
        current: String,
        choices: Vec<PluginChoice>,
        field: PluginField,
        id: &'static str,
        empty: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let this = cx.entity();
        let button_label = if current.trim().is_empty() {
            empty.to_string()
        } else {
            current.clone()
        };
        let disabled = choices.is_empty();
        let menu_current = current.clone();
        let button = Button::new(id)
            .small()
            .outline()
            .label(format!("{button_label} v"))
            .when(disabled, |button| button.disabled(true))
            .dropdown_menu(move |mut menu, _window, _cx| {
                for choice in choices.iter() {
                    let value = choice.name.clone();
                    let menu_label = if choice.installed {
                        value.clone()
                    } else {
                        format!("{value} (preset)")
                    };
                    let this = this.clone();
                    let checked = value == menu_current;
                    menu = menu.item(PopupMenuItem::new(menu_label).checked(checked).on_click(
                        move |_e, window, app| {
                            match field {
                                PluginField::Connector => this.update(app, |dialog, cx| {
                                    dialog.apply_connector_selection(value.clone(), window, cx);
                                }),
                                PluginField::Os => this.update(app, |dialog, cx| {
                                    dialog.apply_os_selection(value.clone(), window, cx);
                                }),
                            };
                        },
                    ));
                }
                menu
            });

        gpui_component::v_flex()
            .gap(px(tokens::space::XS))
            .child(modal::field_label(label, cx))
            .child(button)
            .into_any_element()
    }

    fn render_plugin_state(&self, name: &str, installed: bool, cx: &App) -> AnyElement {
        let text = if name.trim().is_empty() {
            "Select a connector".to_string()
        } else if installed {
            format!("{name} installed")
        } else if connector_preset(name).is_some() {
            format!("{name} preset; plugin not discovered")
        } else {
            format!("{name} custom")
        };
        div()
            .text_size(px(tokens::font::UI_XS))
            .text_color(color::text_muted(cx))
            .child(text)
            .into_any_element()
    }

    fn render_install_button(&self, cx: &mut Context<Self>) -> AnyElement {
        Button::new("memflow-install-standard")
            .small()
            .label(if self.installing_plugins {
                "Installing..."
            } else {
                "Install Plugins"
            })
            .when(self.installing_plugins, |button| button.disabled(true))
            .on_click(cx.listener(|this, _e, _window, cx| this.request_standard_plugin_install(cx)))
            .into_any_element()
    }

    fn render_install_confirmation(
        &self,
        confirmation: InstallConfirmation,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let after_text = match confirmation.after {
            PendingInstallAfter::Connect => {
                "After installation, the inventory will refresh and Connect will continue."
            }
            PendingInstallAfter::Stay => "After installation, the inventory will refresh.",
        };
        let accept_label = match confirmation.after {
            PendingInstallAfter::Connect => "Install and Connect",
            PendingInstallAfter::Stay => "Install",
        };
        let plugins = STANDARD_MEMFLOW_PLUGINS.join(", ");

        div()
            .absolute()
            .inset_0()
            .size_full()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .p(px(tokens::space::XL))
            .bg(gpui::hsla(0., 0., 0., 0.45))
            .on_mouse_down(MouseButton::Left, |_, _, cx: &mut App| {
                cx.stop_propagation()
            })
            .child(
                gpui_component::v_flex()
                    .id("memflow-install-confirmation")
                    .w_full()
                    .max_w(px(620.))
                    .gap(px(tokens::space::SM))
                    .p(px(tokens::space::MD))
                    .rounded(px(tokens::radius::LG))
                    .border_1()
                    .border_color(color::warning(cx))
                    .bg(color::elevated_bg(cx))
                    .shadow_lg()
                    .child(
                        div()
                            .text_size(px(tokens::font::UI_MD))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(color::text(cx))
                            .child("Install memflow plugins?"),
                    )
                    .child(
                        div()
                            .text_size(px(tokens::font::UI_SM))
                            .text_color(color::text(cx))
                            .child(confirmation.reason),
                    )
                    .child(
                        div()
                            .text_size(px(tokens::font::UI_SM))
                            .text_color(color::text_muted(cx))
                            .child(format!(
                                "This will check `cargo --version`, install `memflowup` with `cargo install memflowup --force` if needed, then run `memflowup pull` for: {plugins}."
                            )),
                    )
                    .child(
                        gpui_component::v_flex()
                            .gap(px(tokens::space::XXS))
                            .child(
                                div()
                                    .text_size(px(tokens::font::UI_SM))
                                    .text_color(color::text_muted(cx))
                                    .child("Plugins will be installed into:"),
                            )
                            .child(
                                div()
                                    .text_size(px(tokens::font::UI_XS))
                                    .text_color(color::text(cx))
                                    .font_family(tokens::font::mono_family())
                                    .child(confirmation.install_dir),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(tokens::font::UI_SM))
                            .text_color(color::text_muted(cx))
                            .child(
                                "Cargo must be installed and available on PATH. If Cargo is missing, this operation will fail.",
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(tokens::font::UI_SM))
                            .text_color(color::text_muted(cx))
                            .child(after_text),
                    )
                    .child(
                        gpui_component::h_flex()
                            .w_full()
                            .justify_end()
                            .gap(px(tokens::space::SM))
                            .child(
                                Button::new("memflow-install-cancel")
                                    .small()
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _e, _window, cx| {
                                        this.cancel_install_confirmation(cx)
                                    })),
                            )
                            .child(
                                Button::new("memflow-install-confirm")
                                    .small()
                                    .primary()
                                    .label(accept_label)
                                    .on_click(cx.listener(|this, _e, _window, cx| {
                                        this.confirm_install(cx)
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_status_pill(&self, cx: &App) -> AnyElement {
        let (label, fg) = if self.connected {
            ("Connected", color::success(cx))
        } else {
            ("Disconnected", color::text_muted(cx))
        };
        div()
            .px(px(tokens::space::SM))
            .py(px(tokens::space::XS))
            .rounded(px(tokens::radius::FULL))
            .border_1()
            .border_color(color::border(cx))
            .text_size(px(tokens::font::UI_XS))
            .text_color(fg)
            .child(label)
            .into_any_element()
    }

    fn render_process_stage(&self, cx: &mut Context<Self>) -> AnyElement {
        let count = self.process_table.read(cx).delegate().rows().len();
        let selected = self.selected_process.as_ref().map(|process| {
            format!(
                "{} (pid {}){}",
                process.name,
                process.pid,
                if process.is_32bit { ", 32-bit" } else { "" }
            )
        });

        gpui_component::v_flex()
            .min_w_0()
            .gap(px(tokens::space::MD))
            .child(
                gpui_component::h_flex()
                    .min_w_0()
                    .items_center()
                    .gap(px(tokens::space::MD))
                    .child(
                        div()
                            .text_size(px(tokens::font::UI_SM))
                            .text_color(color::text_muted(cx))
                            .child(format!("{count} process(es)")),
                    )
                    .child(div().flex_1().min_w_0())
                    .child(
                        Button::new("memflow-refresh-processes")
                            .small()
                            .label("Refresh Processes")
                            .on_click(cx.listener(|this, _e, _window, cx| this.connect(cx))),
                    ),
            )
            .child(Input::new(&self.process_filter).small().w_full())
            .child(
                div()
                    .h(px(250.))
                    .w_full()
                    .rounded(px(tokens::radius::LG))
                    .border_1()
                    .border_color(color::border(cx))
                    .overflow_hidden()
                    .child(DataTable::new(&self.process_table).bordered(false).small()),
            )
            .when_some(selected, |body, selected| {
                body.child(
                    div()
                        .text_size(px(tokens::font::UI_SM))
                        .text_color(color::text_muted(cx))
                        .child(format!("Selected: {selected}")),
                )
            })
            .child(
                gpui_component::h_flex()
                    .min_w_0()
                    .gap(px(tokens::space::MD))
                    .child(div().flex_1().min_w_0().child(self.render_input(
                        "Manual PID",
                        &self.manual_pid,
                        cx,
                    )))
                    .child(div().flex_1().min_w_0().child(self.render_input(
                        "Manual Process Name",
                        &self.manual_process_name,
                        cx,
                    ))),
            )
            .into_any_element()
    }

    fn render_activity_log(&self, cx: &App) -> AnyElement {
        let mut list = gpui_component::v_flex()
            .id("memflow-activity-log")
            .w_full()
            .gap(px(tokens::space::XS))
            .max_h(px(104.))
            .overflow_y_scroll();
        for line in self.activity.iter().rev().take(8) {
            list = list.child(
                div()
                    .text_size(px(tokens::font::UI_XS))
                    .text_color(color::text_muted(cx))
                    .font_family(tokens::font::mono_family())
                    .child(line.clone()),
            );
        }
        gpui_component::v_flex()
            .w_full()
            .gap(px(tokens::space::XS))
            .child(modal::field_label("Activity", cx))
            .child(
                div()
                    .w_full()
                    .rounded(px(tokens::radius::LG))
                    .border_1()
                    .border_color(color::border(cx))
                    .p(px(tokens::space::SM))
                    .child(list),
            )
            .into_any_element()
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        gpui_component::h_flex()
            .min_w_0()
            .flex_shrink_0()
            .items_center()
            .h(px(52.))
            .border_b_1()
            .border_color(color::border(cx))
            .child(div().w(px(modal::PAD)).flex_none())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(tokens::font::UI_LG))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(color::text(cx))
                    .child("Attach Process Memory"),
            )
            .child(modal::close_button(
                "memflow-close",
                cx.listener(|this, _e, _window, cx| this.cancel(cx)),
                cx,
            ))
            .child(div().w(px(modal::PAD)).flex_none())
            .into_any_element()
    }

    fn render_body(&self, content: AnyElement, cx: &App) -> AnyElement {
        let _ = cx;
        gpui_component::v_flex()
            .w_full()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(div().h(px(modal::PAD)).flex_none())
            .child(
                gpui_component::h_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .items_stretch()
                    .child(div().w(px(modal::PAD)).flex_none())
                    .child(
                        div()
                            .id("memflow-attach-scroll")
                            .flex_1()
                            .h_full()
                            .min_h_0()
                            .min_w_0()
                            .overflow_y_scroll()
                            .child(content),
                    )
                    .child(div().w(px(modal::PAD)).flex_none()),
            )
            .child(div().h(px(modal::PAD)).flex_none())
            .into_any_element()
    }

    fn render_footer(
        &self,
        card_w: Pixels,
        can_attach: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let gap = f32::from(tokens::space::MD);
        let pad = modal::PAD;
        let cancel_w = 88.;
        let connect_w = 116.;
        let attach_w = 88.;
        let footer_w = f32::from(card_w);
        let attach_left = footer_w - pad - attach_w;
        let connect_left = if can_attach {
            attach_left - gap - connect_w
        } else {
            footer_w - pad - connect_w
        };
        let cancel_left = if can_attach {
            connect_left - gap - cancel_w
        } else {
            connect_left - gap - cancel_w
        };

        div()
            .w_full()
            .min_w_0()
            .relative()
            .flex_shrink_0()
            .h(px(54.))
            .border_t_1()
            .border_color(color::border(cx))
            .child(
                div()
                    .id("memflow-cancel-footer")
                    .absolute()
                    .top(px(12.))
                    .left(px(cancel_left))
                    .w(px(cancel_w))
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(tokens::radius::MD))
                    .border_1()
                    .border_color(color::border(cx))
                    .bg(color::elevated_bg(cx))
                    .text_size(px(tokens::font::UI_SM))
                    .text_color(color::text(cx))
                    .hover(|s| s.bg(color::hover_overlay(cx)))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _e, _window, cx| this.cancel(cx)))
                    .child("Cancel"),
            )
            .child(
                div()
                    .id("memflow-connect-footer")
                    .absolute()
                    .top(px(12.))
                    .left(px(connect_left))
                    .w(px(connect_w))
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(tokens::radius::MD))
                    .border_1()
                    .border_color(if can_attach {
                        color::border(cx)
                    } else {
                        color::accent(cx)
                    })
                    .bg(if can_attach {
                        color::elevated_bg(cx)
                    } else {
                        color::accent(cx)
                    })
                    .text_size(px(tokens::font::UI_SM))
                    .text_color(if self.installing_plugins {
                        color::text_disabled(cx)
                    } else if can_attach {
                        color::text(cx)
                    } else {
                        cx.theme().primary_foreground
                    })
                    .when(!self.installing_plugins && can_attach, |button| {
                        button.hover(|s| s.bg(color::hover_overlay(cx)))
                    })
                    .when(!self.installing_plugins, |button| {
                        button
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _e, _window, cx| {
                                if !this.installing_plugins {
                                    this.connect(cx);
                                }
                            }))
                    })
                    .child(if self.connected {
                        "Reconnect"
                    } else {
                        "Connect"
                    }),
            )
            .when(can_attach, |footer| {
                footer.child(
                    div()
                        .id("memflow-attach-footer")
                        .absolute()
                        .top(px(12.))
                        .left(px(attach_left))
                        .w(px(attach_w))
                        .h(px(28.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(tokens::radius::MD))
                        .border_1()
                        .border_color(color::accent(cx))
                        .bg(color::accent(cx))
                        .text_size(px(tokens::font::UI_SM))
                        .text_color(cx.theme().primary_foreground)
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _e, _window, cx| this.attach(cx)))
                        .child("Attach"),
                )
            })
            .into_any_element()
    }
}

impl Focusable for MemflowAttachDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.connector.read(cx).focus_handle(cx)
    }
}

impl EventEmitter<MemflowAttachEvent> for MemflowAttachDialog {}

impl Render for MemflowAttachDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let card_w = modal::clamp_width(MEMFLOW_DIALOG_WIDTH, window);
        let card_h = modal::clamp_height(720., 80., window);
        let can_attach = self.can_attach(cx);
        let connector_choices =
            plugin_choices(&self.inventory.connectors, preset_connector_names());
        let os_choices = plugin_choices(&self.inventory.os_layers, STANDARD_OS_LAYERS);
        let connector = Self::text(&self.connector, cx);
        let os = Self::text(&self.os, cx);
        let connector_installed = self
            .inventory
            .connectors
            .iter()
            .any(|name| name.eq_ignore_ascii_case(connector.trim()));

        let content = gpui_component::v_flex()
            .w_full()
            .min_w_0()
            .gap(px(tokens::space::LG))
            .child(
                gpui_component::h_flex()
                    .min_w_0()
                    .items_center()
                    .gap(px(tokens::space::MD))
                    .child(self.render_status_pill(cx))
                    .when(!self.status.is_empty(), |row| {
                        row.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(tokens::font::UI_SM))
                                .text_color(color::text_muted(cx))
                                .child(self.status.clone()),
                        )
                    })
                    .when(self.status.is_empty(), |row| row.child(div().flex_1())),
            )
            .child(
                gpui_component::h_flex()
                    .min_w_0()
                    .gap(px(tokens::space::MD))
                    .child(
                        div().w(px(180.)).flex_none().child(
                            gpui_component::v_flex()
                                .gap(px(tokens::space::XS))
                                .child(self.render_plugin_dropdown(
                                    "Connector",
                                    connector.clone(),
                                    connector_choices,
                                    PluginField::Connector,
                                    "memflow-connector-dropdown",
                                    "Select connector",
                                    cx,
                                ))
                                .child(self.render_plugin_state(
                                    &connector,
                                    connector_installed,
                                    cx,
                                )),
                        ),
                    )
                    .child(div().flex_1().min_w_0().child(self.render_input(
                        "Connector Name",
                        &self.connector,
                        cx,
                    )))
                    .child(div().flex_1().min_w_0().child(self.render_input(
                        "Connector Args",
                        &self.connector_args,
                        cx,
                    ))),
            )
            .child(
                gpui_component::h_flex()
                    .min_w_0()
                    .gap(px(tokens::space::MD))
                    .child(
                        div()
                            .w(px(180.))
                            .flex_none()
                            .child(self.render_plugin_dropdown(
                                "OS",
                                os.clone(),
                                os_choices,
                                PluginField::Os,
                                "memflow-os-dropdown",
                                "Select OS",
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(self.render_input("OS Name", &self.os, cx)),
                    )
                    .child(div().flex_1().min_w_0().child(self.render_input(
                        "OS Args",
                        &self.os_args,
                        cx,
                    ))),
            )
            .child(self.render_input("Plugin Dirs", &self.plugin_dirs, cx))
            .child(
                gpui_component::v_flex()
                    .min_w_0()
                    .gap(px(tokens::space::MD))
                    .child(
                        gpui_component::h_flex().min_w_0().items_center().child(
                            Checkbox::new("memflow-writable")
                                .label("Allow writes")
                                .checked(self.writable)
                                .on_click(cx.listener(|this, on: &bool, _window, cx| {
                                    this.writable = *on;
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(
                        gpui_component::h_flex()
                            .min_w_0()
                            .items_center()
                            .flex_wrap()
                            .gap(px(tokens::space::MD))
                            .child(self.render_install_button(cx))
                            .child(
                                Button::new("memflow-refresh-inventory")
                                    .small()
                                    .label("Refresh Inventory")
                                    .on_click(cx.listener(|this, _e, _window, cx| {
                                        this.refresh_inventory(cx)
                                    })),
                            ),
                    ),
            )
            .when(self.connected, |body| {
                body.child(self.render_process_stage(cx))
            })
            .child(self.render_activity_log(cx));

        let body = self.render_body(content.into_any_element(), cx);
        let footer = self.render_footer(card_w, can_attach, cx);

        modal::card(cx)
            .id("rcx-memflow-attach")
            .relative()
            .track_focus(&self.focus_handle)
            .key_context("RcxMemflowAttach")
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
                if this.pending_install.is_some() {
                    match ev.keystroke.key.as_str() {
                        "enter" => {
                            this.confirm_install(cx);
                            cx.stop_propagation();
                        }
                        "escape" => {
                            this.cancel_install_confirmation(cx);
                            cx.stop_propagation();
                        }
                        _ => {}
                    }
                    return;
                }
                match ev.keystroke.key.as_str() {
                    "enter" => {
                        if this.can_attach(cx) {
                            this.attach(cx);
                        } else {
                            this.connect(cx);
                        }
                        cx.stop_propagation();
                    }
                    "escape" => {
                        this.cancel(cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .w(card_w)
            .h(card_h)
            .child(self.render_header(cx))
            .child(body)
            .child(footer)
            .when_some(self.pending_install.clone(), |card, confirmation| {
                card.child(self.render_install_confirmation(confirmation, cx))
            })
    }
}

fn input(
    cx: &mut Context<MemflowAttachDialog>,
    window: &mut Window,
    placeholder: &'static str,
    value: String,
) -> Entity<InputState> {
    cx.new(|cx| {
        let mut input = InputState::new(window, cx).placeholder(placeholder);
        if !value.is_empty() {
            input.set_value(value, window, cx);
        }
        input
    })
}

fn inventory_status(prefix: &str, info: &MemflowInventoryInfo) -> String {
    if info.warnings.is_empty() {
        format!(
            "{prefix}: {} connector(s), {} OS plugin(s)",
            info.connectors.len(),
            info.os_layers.len()
        )
    } else {
        format!(
            "{prefix}: {} connector(s), {} OS plugin(s), {} warning(s)",
            info.connectors.len(),
            info.os_layers.len(),
            info.warnings.len()
        )
    }
}

fn connector_preset(name: &str) -> Option<&'static ConnectorPreset> {
    let wanted = name.trim();
    CONNECTOR_PRESETS
        .iter()
        .find(|preset| preset.name.eq_ignore_ascii_case(wanted))
}

fn preset_connector_names() -> &'static [&'static str] {
    &["qemu", "kvm", "pcileech", "coredump", "winio"]
}

fn plugin_choices(installed: &[String], presets: &[&str]) -> Vec<PluginChoice> {
    let mut choices = Vec::new();
    for name in installed {
        choices.push(PluginChoice {
            name: name.clone(),
            installed: true,
        });
    }
    for preset in presets {
        if !choices
            .iter()
            .any(|choice| choice.name.eq_ignore_ascii_case(preset))
        {
            choices.push(PluginChoice {
                name: (*preset).to_string(),
                installed: false,
            });
        }
    }
    choices
}

fn inventory_has(installed: &[String], name: &str) -> bool {
    installed
        .iter()
        .any(|installed| installed.eq_ignore_ascii_case(name.trim()))
}

fn standard_memflow_plugin(name: &str) -> bool {
    let name = name.trim();
    STANDARD_MEMFLOW_PLUGINS
        .iter()
        .any(|plugin| plugin.eq_ignore_ascii_case(name))
}

fn command_step(program: &str, args: &[&str]) -> CommandStepResult {
    let command = if args.is_empty() {
        program.to_string()
    } else {
        format!("{program} {}", args.join(" "))
    };
    match Command::new(program).args(args).output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = if output.status.success() {
                first_nonempty_line(&stdout)
                    .or_else(|| first_nonempty_line(&stderr))
                    .unwrap_or_else(|| "ok".to_string())
            } else {
                first_nonempty_line(&stderr)
                    .or_else(|| first_nonempty_line(&stdout))
                    .unwrap_or_else(|| format!("exit status {}", output.status))
            };
            CommandStepResult {
                command,
                ok: output.status.success(),
                missing: false,
                detail,
            }
        }
        Err(err) => CommandStepResult {
            command,
            ok: false,
            missing: err.kind() == ErrorKind::NotFound,
            detail: err.to_string(),
        },
    }
}

fn first_nonempty_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

fn command_log_line(result: &CommandStepResult) -> String {
    let status = if result.ok { "ok" } else { "failed" };
    format!("{}: {status}: {}", result.command, result.detail)
}

fn preferred_connector(connectors: &[String]) -> String {
    for preferred in ["qemu", "kvm", "pcileech", "winio"] {
        if connectors.iter().any(|c| c == preferred) {
            return preferred.to_string();
        }
    }
    connectors.first().cloned().unwrap_or_default()
}
