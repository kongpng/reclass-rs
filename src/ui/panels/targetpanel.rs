//! Target dock — detailed session/provider inspector for the active source.
//!
//! The bottom status bar keeps the compact health truth visible. This panel owns
//! the expanded diagnostics: capabilities, counts, saved-source reattach state,
//! and provider-specific process/paging facts when the provider exposes them.

use gpui::SharedString;

/// Stable `panel_name` for layout (de)serialization.
pub const PANEL_NAME: &str = "TargetPanel";

pub fn title() -> SharedString {
    SharedString::from("Target")
}

#[cfg(feature = "ui")]
pub use view::TargetPanel;

#[cfg(feature = "ui")]
mod view {
    use std::sync::Arc;

    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::dock::{Panel, PanelEvent};
    use gpui_component::tooltip::Tooltip;
    use gpui_component::{ActiveTheme as _, Sizable as _};

    use crate::provider::{MemoryRegion, ModuleEntry, Provider, ThreadInfo};
    use crate::ui::design::{color, icon, tokens};
    use crate::ui::target_status::{TargetHealth, TargetStatusSummary};

    pub struct TargetPanel {
        provider: Option<Arc<dyn Provider + Send + Sync>>,
        summary: TargetStatusSummary,
        focus_handle: FocusHandle,
    }

    impl TargetPanel {
        pub fn new(cx: &mut Context<Self>) -> Self {
            TargetPanel {
                provider: None,
                summary: TargetStatusSummary::no_source(),
                focus_handle: cx.focus_handle(),
            }
        }

        pub fn view(_window: &mut Window, cx: &mut App) -> Entity<Self> {
            cx.new(TargetPanel::new)
        }

        pub fn set_target(
            &mut self,
            provider: Option<Arc<dyn Provider + Send + Sync>>,
            summary: TargetStatusSummary,
        ) {
            self.provider = provider;
            self.summary = summary;
        }
    }

    impl Panel for TargetPanel {
        fn panel_name(&self) -> &'static str {
            super::PANEL_NAME
        }

        fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            super::title()
        }
    }

    impl EventEmitter<PanelEvent> for TargetPanel {}

    impl Focusable for TargetPanel {
        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.focus_handle.clone()
        }
    }

    impl Render for TargetPanel {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let summary = self.summary.clone();
            let details = self
                .provider
                .as_ref()
                .map(|p| TargetDetails::from_provider(p.as_ref()));

            gpui_component::v_flex()
                .id("rcx-target-panel")
                .track_focus(&self.focus_handle)
                .size_full()
                .bg(color::panel_bg(cx))
                .text_color(color::text(cx))
                .child(render_header(&summary, cx))
                .child(
                    div().flex_1().min_h_0().child(match details {
                        Some(details) if summary.health() != TargetHealth::NoSource => {
                            render_body(&summary, details, cx).into_any_element()
                        }
                        _ => crate::ui::design::empty_state(
                            icon::source(),
                            "Attach a source to inspect target health",
                            cx,
                        )
                        .into_any_element(),
                    }),
                )
        }
    }

    #[derive(Clone, Debug, Default)]
    struct TargetDetails {
        modules: Vec<ModuleEntry>,
        regions: Vec<MemoryRegion>,
        peb: u64,
        tebs: Vec<ThreadInfo>,
        has_kernel_paging: bool,
        cr3: u64,
    }

    impl TargetDetails {
        fn from_provider(provider: &dyn Provider) -> Self {
            TargetDetails {
                modules: provider.enumerate_modules(),
                regions: provider.enumerate_regions(),
                peb: provider.peb(),
                tebs: provider.tebs(),
                has_kernel_paging: provider.has_kernel_paging(),
                cr3: if provider.has_kernel_paging() {
                    provider.get_cr3()
                } else {
                    0
                },
            }
        }
    }

    fn render_header(summary: &TargetStatusSummary, cx: &App) -> impl IntoElement {
        crate::ui::design::panel_header_strip(cx)
            .child(
                gpui_component::h_flex()
                    .gap(px(tokens::space::SM))
                    .items_center()
                    .child(
                        icon::source()
                            .with_size(px(13.0))
                            .text_color(color::text_muted(cx)),
                    )
                    .child(
                        div()
                            .text_size(px(tokens::font::UI_SM))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(color::text(cx))
                            .child("Target"),
                    ),
            )
            .child(health_badge(summary, cx))
    }

    fn render_body(
        summary: &TargetStatusSummary,
        details: TargetDetails,
        cx: &App,
    ) -> impl IntoElement {
        let module_count = details.modules.len();
        let region_count = details.regions.len();
        let readable_regions = details.regions.iter().filter(|r| r.readable).count();
        let writable_regions = details.regions.iter().filter(|r| r.writable).count();
        let executable_regions = details.regions.iter().filter(|r| r.executable).count();

        crate::ui::design::panel_list("rcx-target-list")
            .gap(px(tokens::space::MD))
            .children(vec![
                section(
                    "Overview",
                    vec![
                        ("Provider", summary.provider_kind.clone()),
                        ("Target", summary.target_label.clone()),
                        ("Health", summary.health_label().to_string()),
                        ("Access", summary.access_label().to_string()),
                        ("Pointer", summary.pointer_label()),
                        ("View Class", empty_as_dash(&summary.view_class)),
                        ("View Base", summary.view_base_label()),
                        ("View Formula", empty_as_dash(&summary.view_formula)),
                        ("Provider Base", summary.base_label()),
                        ("Size", summary.size_label()),
                    ],
                    cx,
                ),
                section(
                    "Capabilities",
                    vec![
                        ("Read", bool_label(summary.valid)),
                        ("Write", bool_label(summary.writable)),
                        ("Live", bool_label(summary.live)),
                        ("Modules", count_label(module_count)),
                        ("Regions", count_label(region_count)),
                        ("Paging", bool_label(details.has_kernel_paging)),
                    ],
                    cx,
                ),
                section(
                    "Regions",
                    vec![
                        ("Total", count_label(region_count)),
                        ("Readable", count_label(readable_regions)),
                        ("Writable", count_label(writable_regions)),
                        ("Executable", count_label(executable_regions)),
                    ],
                    cx,
                ),
                section(
                    "Process",
                    vec![
                        ("PEB", hex_or_unavailable(details.peb)),
                        ("TEBs", count_label(details.tebs.len())),
                        ("Modules", count_label(module_count)),
                    ],
                    cx,
                ),
                section(
                    "Paging",
                    vec![
                        ("CR3", hex_or_unavailable(details.cr3)),
                        (
                            "VTOP",
                            if details.has_kernel_paging {
                                "available".to_string()
                            } else {
                                "unavailable".to_string()
                            },
                        ),
                    ],
                    cx,
                ),
                section(
                    "Saved Source",
                    vec![
                        ("Kind", empty_as_dash(&summary.saved_kind)),
                        ("Target", empty_as_dash(&summary.saved_target)),
                        ("Reattach", summary.reattach_label().to_string()),
                    ],
                    cx,
                ),
                section(
                    "Probes",
                    vec![
                        ("Last read", "not probed".to_string()),
                        ("Last write", "not probed".to_string()),
                    ],
                    cx,
                ),
            ])
            .when(!details.modules.is_empty(), |list| {
                list.child(module_preview(details.modules, cx))
            })
    }

    fn section(title: &'static str, rows: Vec<(&'static str, String)>, cx: &App) -> AnyElement {
        gpui_component::v_flex()
            .w_full()
            .gap(px(tokens::space::XXS))
            .child(crate::ui::design::section_label(title, cx))
            .children(
                rows.into_iter()
                    .map(|(label, value)| kv_row(label, value, cx)),
            )
            .into_any_element()
    }

    fn kv_row(label: &'static str, value: String, cx: &App) -> impl IntoElement {
        gpui_component::h_flex()
            .w_full()
            .min_h(px(22.0))
            .gap(px(tokens::space::MD))
            .items_center()
            .justify_between()
            .child(
                div()
                    .flex_none()
                    .text_size(px(tokens::font::UI_SM))
                    .text_color(color::text_muted(cx))
                    .child(label),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_align(TextAlign::Right)
                    .text_size(px(tokens::font::UI_SM))
                    .text_color(color::text(cx))
                    .child(value),
            )
    }

    fn module_preview(modules: Vec<ModuleEntry>, cx: &App) -> AnyElement {
        let mut modules = modules;
        modules.sort_by_key(|m| m.base);
        let rows: Vec<AnyElement> = modules
            .into_iter()
            .take(6)
            .enumerate()
            .map(|(ix, m)| {
                let name = if m.name.is_empty() {
                    "(unnamed)".to_string()
                } else {
                    m.name
                };
                let tooltip = format!("{} · base 0x{:X} · size 0x{:X}", name, m.base, m.size);
                gpui_component::h_flex()
                    .id(("rcx-target-module-preview", ix))
                    .h(px(22.0))
                    .w_full()
                    .gap(px(tokens::space::MD))
                    .items_center()
                    .child(
                        icon::source()
                            .with_size(px(11.0))
                            .text_color(color::text_muted(cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(tokens::font::UI_SM))
                            .text_color(color::text(cx))
                            .child(name),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_family(tokens::font::mono_family())
                            .text_size(px(tokens::font::UI_XS))
                            .text_color(color::syntax_address(cx))
                            .child(format!("0x{:X}", m.base)),
                    )
                    .tooltip(move |window, cx| {
                        Tooltip::new(SharedString::from(tooltip.clone())).build(window, cx)
                    })
                    .into_any_element()
            })
            .collect();
        gpui_component::v_flex()
            .w_full()
            .gap(px(tokens::space::XXS))
            .child(crate::ui::design::section_label("Modules Preview", cx))
            .children(rows)
            .into_any_element()
    }

    fn health_badge(summary: &TargetStatusSummary, cx: &App) -> impl IntoElement {
        let fg = match summary.health() {
            TargetHealth::Live => cx.theme().green,
            TargetHealth::Static => color::text_muted(cx),
            TargetHealth::Offline => cx.theme().red,
            TargetHealth::NoSource => color::text_disabled(cx),
        };
        gpui_component::h_flex()
            .flex_none()
            .h(px(22.0))
            .px(px(tokens::space::SM))
            .gap(px(tokens::space::XS))
            .items_center()
            .rounded(px(tokens::radius::MD))
            .bg(color::hover_overlay(cx))
            .text_size(px(tokens::font::UI_XS))
            .text_color(fg)
            .child("●")
            .child(summary.health_label())
    }

    fn bool_label(value: bool) -> String {
        if value {
            "yes".to_string()
        } else {
            "no".to_string()
        }
    }

    fn count_label(value: usize) -> String {
        value.to_string()
    }

    fn hex_or_unavailable(value: u64) -> String {
        if value == 0 {
            "unavailable".to_string()
        } else {
            format!("0x{value:X}")
        }
    }

    fn empty_as_dash(value: &str) -> String {
        if value.trim().is_empty() {
            "-".to_string()
        } else {
            value.to_string()
        }
    }
}
