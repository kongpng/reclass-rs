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

    use crate::provider::{ModuleEntry, Provider};
    use crate::ui::design::{color, icon, tokens};
    use crate::ui::target_status::{TargetHealth, TargetStatusSummary};

    pub struct TargetPanel {
        provider: Option<Arc<dyn Provider + Send + Sync>>,
        summary: TargetStatusSummary,
        details: Option<TargetDetails>,
        focus_handle: FocusHandle,
    }

    impl TargetPanel {
        pub fn new(cx: &mut Context<Self>) -> Self {
            TargetPanel {
                provider: None,
                summary: TargetStatusSummary::no_source(),
                details: None,
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
            self.refresh_details();
        }

        fn refresh_details(&mut self) {
            self.details = self
                .provider
                .as_ref()
                .map(|provider| TargetDetails::from_provider(provider.as_ref()));
        }

        fn refresh_details_notify(&mut self, cx: &mut Context<Self>) {
            self.refresh_details();
            cx.notify();
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
            if self.provider.is_some() && self.details.is_none() {
                self.refresh_details();
            }
            let details = self.details.as_ref();

            gpui_component::v_flex()
                .id("rcx-target-panel")
                .track_focus(&self.focus_handle)
                .size_full()
                .bg(color::panel_bg(cx))
                .text_color(color::text(cx))
                .child(render_header(&summary, &cx.entity(), cx))
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
        region_count: usize,
        readable_regions: usize,
        writable_regions: usize,
        executable_regions: usize,
        peb: u64,
        tebs_count: usize,
        has_kernel_paging: bool,
        cr3: u64,
    }

    impl TargetDetails {
        fn from_provider(provider: &dyn Provider) -> Self {
            let mut modules = provider.enumerate_modules();
            modules.sort_by_key(|m| m.base);
            let regions = provider.enumerate_regions();
            let readable_regions = regions.iter().filter(|r| r.readable).count();
            let writable_regions = regions.iter().filter(|r| r.writable).count();
            let executable_regions = regions.iter().filter(|r| r.executable).count();
            let has_kernel_paging = provider.has_kernel_paging();
            TargetDetails {
                modules,
                region_count: regions.len(),
                readable_regions,
                writable_regions,
                executable_regions,
                peb: provider.peb(),
                tebs_count: provider.tebs().len(),
                has_kernel_paging,
                cr3: if has_kernel_paging {
                    provider.get_cr3()
                } else {
                    0
                },
            }
        }
    }

    fn render_header(
        summary: &TargetStatusSummary,
        view: &Entity<TargetPanel>,
        cx: &Context<TargetPanel>,
    ) -> impl IntoElement {
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
            .child({
                let view = view.clone();
                refresh_target_button(cx).on_click(move |_e, _w, cx| {
                    view.update(cx, |this, cx| this.refresh_details_notify(cx));
                })
            })
            .child(health_badge(summary, cx))
    }

    fn render_body(
        summary: &TargetStatusSummary,
        details: &TargetDetails,
        cx: &App,
    ) -> impl IntoElement {
        let module_count = details.modules.len();

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
                        ("Regions", count_label(details.region_count)),
                        ("Paging", bool_label(details.has_kernel_paging)),
                    ],
                    cx,
                ),
                section(
                    "Regions",
                    vec![
                        ("Total", count_label(details.region_count)),
                        ("Readable", count_label(details.readable_regions)),
                        ("Writable", count_label(details.writable_regions)),
                        ("Executable", count_label(details.executable_regions)),
                    ],
                    cx,
                ),
                section(
                    "Process",
                    vec![
                        ("PEB", hex_or_unavailable(details.peb)),
                        ("TEBs", count_label(details.tebs_count)),
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
                list.child(module_preview(&details.modules, cx))
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

    fn module_preview(modules: &[ModuleEntry], cx: &App) -> AnyElement {
        let rows: Vec<AnyElement> = modules
            .iter()
            .take(6)
            .enumerate()
            .map(|(ix, m)| {
                let name = if m.name.is_empty() {
                    "(unnamed)".to_string()
                } else {
                    m.name.clone()
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

    fn refresh_target_button(cx: &App) -> Stateful<Div> {
        div()
            .id("rcx-target-refresh")
            .flex_none()
            .size(px(22.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(tokens::radius::MD))
            .text_color(color::text_muted(cx))
            .hover(|s| s.bg(color::hover_overlay(cx)).text_color(color::text(cx)))
            .tooltip(|window, cx| Tooltip::new("Refresh target details").build(window, cx))
            .child(icon::refresh().with_size(px(12.0)))
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

    #[cfg(test)]
    mod tests {
        use super::TargetDetails;
        use crate::provider::{MemoryRegion, ModuleEntry, Provider, RegionType};
        use std::cell::Cell;

        struct CountingTargetProvider {
            module_calls: Cell<usize>,
            region_calls: Cell<usize>,
        }

        impl CountingTargetProvider {
            fn new() -> Self {
                Self {
                    module_calls: Cell::new(0),
                    region_calls: Cell::new(0),
                }
            }
        }

        impl Provider for CountingTargetProvider {
            fn read(&self, _addr: u64, _buf: &mut [u8]) -> bool {
                false
            }

            fn size(&self) -> i32 {
                1
            }

            fn enumerate_modules(&self) -> Vec<ModuleEntry> {
                self.module_calls.set(self.module_calls.get() + 1);
                vec![
                    ModuleEntry {
                        name: "late.dll".into(),
                        full_path: String::new(),
                        base: 0x4000,
                        size: 0x100,
                    },
                    ModuleEntry {
                        name: "early.dll".into(),
                        full_path: String::new(),
                        base: 0x1000,
                        size: 0x100,
                    },
                ]
            }

            fn enumerate_regions(&self) -> Vec<MemoryRegion> {
                self.region_calls.set(self.region_calls.get() + 1);
                vec![
                    MemoryRegion {
                        base: 0x1000,
                        size: 0x100,
                        readable: true,
                        writable: false,
                        executable: true,
                        module_name: "early.dll".into(),
                        region_type: RegionType::Image,
                    },
                    MemoryRegion {
                        base: 0x2000,
                        size: 0x100,
                        readable: true,
                        writable: true,
                        executable: false,
                        module_name: String::new(),
                        region_type: RegionType::Private,
                    },
                ]
            }
        }

        #[test]
        fn target_details_snapshot_enumerates_once_and_precomputes_render_counts() {
            let provider = CountingTargetProvider::new();

            let details = TargetDetails::from_provider(&provider);

            assert_eq!(provider.module_calls.get(), 1);
            assert_eq!(provider.region_calls.get(), 1);
            assert_eq!(
                details.modules.iter().map(|m| m.base).collect::<Vec<_>>(),
                vec![0x1000, 0x4000]
            );
            assert_eq!(details.region_count, 2);
            assert_eq!(details.readable_regions, 2);
            assert_eq!(details.writable_regions, 1);
            assert_eq!(details.executable_regions, 1);
        }
    }
}
