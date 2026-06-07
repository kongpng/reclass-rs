//! memflow attach dialog — select connector/OS plugins, optional third-party
//! plugin dirs, process target, and write policy for the `processmemory` provider.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable as _, Sizable as _};

use crate::provider::memflow::{
    enumerate_processes, inventory_info, MemflowAttachConfig, MemflowInventoryInfo,
    MemflowProcessInfo,
};
use crate::ui::design::{color, tokens};
use crate::ui::dialogs::modal;

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
    pid: Entity<InputState>,
    process_name: Entity<InputState>,
    plugin_dirs: Entity<InputState>,
    writable: bool,
    inventory: MemflowInventoryInfo,
    process_rows: Vec<MemflowProcessInfo>,
    status: String,
    focus_handle: FocusHandle,
    _subs: Vec<Subscription>,
}

impl MemflowAttachDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let inventory = inventory_info(&[]);
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
        let connector_args = input(cx, window, "target:extra:middleware", String::new());
        let os = input(cx, window, "win32", os_seed);
        let os_args = input(cx, window, ":dtb=0x...", String::new());
        let pid = input(cx, window, "pid", String::new());
        let process_name = input(cx, window, "process.exe", String::new());
        let plugin_dirs = input(cx, window, "/path/to/plugins; /other/path", String::new());

        let mut subs = Vec::new();
        for input in [
            connector.clone(),
            connector_args.clone(),
            os.clone(),
            os_args.clone(),
            pid.clone(),
            process_name.clone(),
            plugin_dirs.clone(),
        ] {
            subs.push(
                cx.subscribe_in(&input, window, |_this, _input, ev: &InputEvent, _w, cx| {
                    if matches!(ev, InputEvent::Change) {
                        cx.notify();
                    }
                }),
            );
        }

        Self {
            connector,
            connector_args,
            os,
            os_args,
            pid,
            process_name,
            plugin_dirs,
            writable: false,
            inventory,
            process_rows: Vec::new(),
            status: String::new(),
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

    fn plugin_dirs(&self, cx: &App) -> Vec<String> {
        Self::text(&self.plugin_dirs, cx)
            .split([';', '\n'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    fn config(&self, require_process: bool, cx: &App) -> Result<MemflowAttachConfig, String> {
        let pid_text = Self::text(&self.pid, cx);
        let pid = if pid_text.trim().is_empty() {
            None
        } else {
            Some(
                pid_text
                    .trim()
                    .parse::<u32>()
                    .map_err(|_| "PID must be a decimal u32".to_string())?,
            )
        };
        let cfg = MemflowAttachConfig {
            connector: Self::text(&self.connector, cx).trim().to_string(),
            connector_args: Self::text(&self.connector_args, cx).trim().to_string(),
            os: Self::text(&self.os, cx).trim().to_string(),
            os_args: Self::text(&self.os_args, cx).trim().to_string(),
            pid,
            process_name: Self::text(&self.process_name, cx).trim().to_string(),
            writable: self.writable,
            inventory_dirs: self.plugin_dirs(cx),
        };
        if cfg.os.is_empty() {
            return Err("OS plugin is required, usually win32".to_string());
        }
        if require_process {
            cfg.validate()?;
        }
        Ok(cfg)
    }

    fn refresh_inventory(&mut self, cx: &mut Context<Self>) {
        self.inventory = inventory_info(&self.plugin_dirs(cx));
        self.status = if self.inventory.warnings.is_empty() {
            format!(
                "{} connector(s), {} OS plugin(s)",
                self.inventory.connectors.len(),
                self.inventory.os_layers.len()
            )
        } else {
            format!(
                "{} connector(s), {} OS plugin(s); {} warning(s)",
                self.inventory.connectors.len(),
                self.inventory.os_layers.len(),
                self.inventory.warnings.len()
            )
        };
        cx.notify();
    }

    fn list_processes(&mut self, cx: &mut Context<Self>) {
        match self
            .config(false, cx)
            .and_then(|cfg| enumerate_processes(&cfg))
        {
            Ok(rows) => {
                self.status = format!("Found {} process(es)", rows.len());
                self.process_rows = rows;
            }
            Err(err) => {
                self.status = err;
                self.process_rows.clear();
            }
        }
        cx.notify();
    }

    fn use_process(
        &mut self,
        row: &MemflowProcessInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        Self::set_text(&self.pid, row.pid.to_string(), window, cx);
        Self::set_text(&self.process_name, row.name.clone(), window, cx);
        self.status = format!("Selected {} (pid {})", row.name, row.pid);
        cx.notify();
    }

    fn attach(&mut self, cx: &mut Context<Self>) {
        match self.config(true, cx) {
            Ok(cfg) => cx.emit(MemflowAttachEvent::Attach(cfg)),
            Err(err) => {
                self.status = err;
                cx.notify();
            }
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(MemflowAttachEvent::Cancel);
    }

    fn can_attach(&self, cx: &App) -> bool {
        !Self::text(&self.os, cx).trim().is_empty()
            && (!Self::text(&self.pid, cx).trim().is_empty()
                || !Self::text(&self.process_name, cx).trim().is_empty())
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

    fn render_picker_buttons(
        &self,
        title: &'static str,
        names: &[String],
        target: Entity<InputState>,
        empty: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut row = gpui_component::h_flex()
            .w_full()
            .items_center()
            .gap(px(tokens::space::SM))
            .child(
                div()
                    .w(px(84.))
                    .text_size(px(tokens::font::UI_SM))
                    .text_color(color::text_muted(cx))
                    .child(title),
            );
        if names.is_empty() {
            row = row.child(
                div()
                    .text_size(px(tokens::font::UI_SM))
                    .text_color(color::text_muted(cx))
                    .child(empty),
            );
        } else {
            let id_prefix = if title == "Connector" {
                "memflow-connector-plugin"
            } else {
                "memflow-os-plugin"
            };
            for (ix, name) in names.iter().take(8).enumerate() {
                let value = name.clone();
                let target = target.clone();
                row = row.child(
                    Button::new((id_prefix, ix))
                        .small()
                        .label(value.clone())
                        .on_click(cx.listener(move |_this, _e, window, cx| {
                            Self::set_text(&target, value.clone(), window, cx);
                        })),
                );
            }
        }
        row.into_any_element()
    }

    fn render_process_rows(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut list = gpui_component::v_flex().w_full().gap(px(tokens::space::XS));
        if self.process_rows.is_empty() {
            list = list.child(
                div()
                    .text_size(px(tokens::font::UI_SM))
                    .text_color(color::text_muted(cx))
                    .child("No process list loaded"),
            );
        } else {
            for row in self.process_rows.iter().take(10) {
                let row_for_click = row.clone();
                let name = if row.is_32bit {
                    format!("{} (32-bit)", row.name)
                } else {
                    row.name.clone()
                };
                list = list.child(
                    gpui_component::h_flex()
                        .w_full()
                        .items_center()
                        .gap(px(tokens::space::SM))
                        .px(px(tokens::space::SM))
                        .py(px(tokens::space::XS))
                        .rounded(px(tokens::radius::SM))
                        .bg(color::panel_bg(cx))
                        .child(
                            div()
                                .w(px(64.))
                                .font_family(SharedString::from(tokens::font::mono_family()))
                                .text_size(px(tokens::font::UI_SM))
                                .child(row.pid.to_string()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(tokens::font::UI_SM))
                                .child(name),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(tokens::font::UI_XS))
                                .text_color(color::text_muted(cx))
                                .child(row.path.clone()),
                        )
                        .child(
                            Button::new(("memflow-use-pid", row.pid))
                                .small()
                                .label("Use")
                                .on_click(cx.listener(move |this, _e, window, cx| {
                                    this.use_process(&row_for_click, window, cx);
                                })),
                        ),
                );
            }
        }
        list.into_any_element()
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
        let card_w = modal::clamp_width(720., window);
        let card_h = modal::clamp_height(680., 80., window);
        let can_attach = self.can_attach(cx);
        let connector_names = self.inventory.connectors.clone();
        let os_names = self.inventory.os_layers.clone();

        let content = gpui_component::v_flex()
            .w_full()
            .gap(px(tokens::space::LG))
            .child(
                gpui_component::v_flex()
                    .w_full()
                    .gap(px(tokens::space::SM))
                    .child(self.render_picker_buttons(
                        "Connector",
                        &connector_names,
                        self.connector.clone(),
                        "No connectors found",
                        cx,
                    ))
                    .child(self.render_picker_buttons(
                        "OS",
                        &os_names,
                        self.os.clone(),
                        "No OS plugins found",
                        cx,
                    )),
            )
            .child(
                gpui_component::h_flex()
                    .w_full()
                    .gap(px(tokens::space::MD))
                    .child(div().flex_1().child(self.render_input(
                        "Connector",
                        &self.connector,
                        cx,
                    )))
                    .child(div().flex_1().child(self.render_input(
                        "Connector Args",
                        &self.connector_args,
                        cx,
                    ))),
            )
            .child(
                gpui_component::h_flex()
                    .w_full()
                    .gap(px(tokens::space::MD))
                    .child(div().flex_1().child(self.render_input("OS", &self.os, cx)))
                    .child(
                        div()
                            .flex_1()
                            .child(self.render_input("OS Args", &self.os_args, cx)),
                    ),
            )
            .child(
                gpui_component::h_flex()
                    .w_full()
                    .gap(px(tokens::space::MD))
                    .child(
                        div()
                            .flex_1()
                            .child(self.render_input("PID", &self.pid, cx)),
                    )
                    .child(div().flex_1().child(self.render_input(
                        "Process Name",
                        &self.process_name,
                        cx,
                    ))),
            )
            .child(self.render_input("Plugin Dirs", &self.plugin_dirs, cx))
            .child(
                gpui_component::h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(tokens::space::MD))
                    .child(
                        Checkbox::new("memflow-writable")
                            .label("Allow writes")
                            .checked(self.writable)
                            .on_click(cx.listener(|this, on: &bool, _window, cx| {
                                this.writable = *on;
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("memflow-refresh-inventory")
                            .small()
                            .label("Refresh")
                            .on_click(
                                cx.listener(|this, _e, _window, cx| this.refresh_inventory(cx)),
                            ),
                    )
                    .child(
                        Button::new("memflow-list-processes")
                            .small()
                            .label("List Processes")
                            .on_click(cx.listener(|this, _e, _window, cx| this.list_processes(cx))),
                    ),
            )
            .when(!self.status.is_empty(), |body| {
                body.child(
                    div()
                        .text_size(px(tokens::font::UI_SM))
                        .text_color(color::text_muted(cx))
                        .child(self.status.clone()),
                )
            })
            .child(self.render_process_rows(cx));

        let body = modal::body(cx).child(
            div()
                .id("memflow-attach-scroll")
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_y_scroll()
                .child(content),
        );

        let footer = modal::footer(cx)
            .child(
                Button::new("memflow-cancel")
                    .label("Cancel")
                    .on_click(cx.listener(|this, _e, _window, cx| this.cancel(cx))),
            )
            .child(
                Button::new("memflow-attach")
                    .primary()
                    .label("Attach")
                    .when(!can_attach, |button| button.disabled(true))
                    .on_click(cx.listener(|this, _e, _window, cx| this.attach(cx))),
            );

        modal::card(cx)
            .id("rcx-memflow-attach")
            .track_focus(&self.focus_handle)
            .key_context("RcxMemflowAttach")
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
                match ev.keystroke.key.as_str() {
                    "enter" => {
                        this.attach(cx);
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
            .child(modal::header_with_close(
                "Attach Process Memory",
                "memflow-close",
                cx.listener(|this, _e, _window, cx| this.cancel(cx)),
                cx,
            ))
            .child(body)
            .child(footer)
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

fn preferred_connector(connectors: &[String]) -> String {
    for preferred in ["qemu", "kvm", "pcileech", "winio"] {
        if connectors.iter().any(|c| c == preferred) {
            return preferred.to_string();
        }
    }
    connectors.first().cloned().unwrap_or_default()
}
