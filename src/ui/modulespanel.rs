//! Modules dock — the loaded-modules / symbols / types list for the active data
//! source (the C++ View ▸ Modules side panel, `Ctrl+Shift+Y`).
//!
//! Port of the Reclass "Modules" right-side panel (the `data_options.png`
//! top-right cluster: a **Modules / Symbols / Types** tab strip with a
//! **Download All** action, over a list of the modules/images backing the active
//! source — a process's loaded DLLs, or the sections of a file). It resolves
//! addresses to `module+offset` and lets the editor jump a base.
//!
//! Two halves, mirroring the foundation philosophy (gpui-free model + thin view):
//! - [`ModulesTab`] — which list the strip shows (Modules / Symbols / Types),
//!   pure + unit-tested.
//! - [`ModuleRow`] — one module's display strings (name, base, size), built from
//!   a [`ModuleEntry`](crate::provider::ModuleEntry); pure + unit-tested.
//! - [`build_module_rows`] — map the active provider's
//!   [`enumerate_modules`](crate::provider::Provider::enumerate_modules) into the
//!   display rows (sorted by base address), pure + unit-tested.
//! - [`ModulesPanel`] (gated on `ui`) — the Zed-styled gpui-component
//!   [`Panel`](gpui_component::dock::Panel): a header with the tab strip + the
//!   Download All button, over the (virtualization-free, list-sized) module
//!   rows, with a clean empty-state when no source is attached.
//!
//! Symbols + Types are real concepts in Reclass but are not enumerated by the
//! provider seam here, so those tabs render their own empty-state for now; the
//! Modules tab shows the live module list when a provider is attached.
//!
//! Gated behind the `ui` feature.

use gpui::SharedString;

use crate::provider::ModuleEntry;

/// The stable `panel_name` for layout (de)serialization
/// (`DockArea::dump`/`load`). Must stay stable once docks persist it.
pub const PANEL_NAME: &str = "ModulesPanel";

/// The dock's display title (header / tab label).
pub fn title() -> SharedString {
    SharedString::from("Modules")
}

/// Which list the panel's tab strip is showing (the C++ Modules / Symbols /
/// Types tabs in `data_options.png`). Pure; drives both the active-tab styling
/// and which list (or empty-state) the body renders.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ModulesTab {
    /// The loaded-modules / images list (the live one).
    #[default]
    Modules,
    /// Exported / resolved symbols (not yet enumerated here).
    Symbols,
    /// Project / imported types (not yet enumerated here).
    Types,
}

impl ModulesTab {
    /// The tabs in display order (`data_options.png` left→right).
    pub fn all() -> [ModulesTab; 3] {
        [ModulesTab::Modules, ModulesTab::Symbols, ModulesTab::Types]
    }

    /// The tab's strip label.
    pub fn label(self) -> &'static str {
        match self {
            ModulesTab::Modules => "Modules",
            ModulesTab::Symbols => "Symbols",
            ModulesTab::Types => "Types",
        }
    }

    /// The empty-state caption when this tab has no rows (the clean "nothing
    /// here yet" hint a Zed panel shows in place of an empty list).
    pub fn empty_caption(self) -> &'static str {
        match self {
            ModulesTab::Modules => "No modules — attach a data source",
            ModulesTab::Symbols => "No symbols for this source",
            ModulesTab::Types => "No imported types",
        }
    }
}

/// One module row's display strings — the name plus a formatted base address
/// and size (the C++ module-list columns). Built from a
/// [`ModuleEntry`](crate::provider::ModuleEntry); pure + unit-tested.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleRow {
    /// The module name (e.g. `game.exe`), or its full path tail when unnamed.
    pub name: String,
    /// The base address (the row's stable sort key + jump target).
    pub base: u64,
    /// The formatted base address (`0x...`, uppercase hex).
    pub base_text: String,
    /// The formatted size (`0x...` bytes, uppercase hex; empty when zero).
    pub size_text: String,
}

impl ModuleRow {
    /// Build a row from a provider [`ModuleEntry`].
    pub fn from_entry(m: &ModuleEntry) -> ModuleRow {
        let name = if !m.name.is_empty() {
            m.name.clone()
        } else if !m.full_path.is_empty() {
            // Fall back to the path tail (basename) when the name is blank.
            m.full_path
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&m.full_path)
                .to_string()
        } else {
            "(unnamed)".to_string()
        };
        ModuleRow {
            name,
            base: m.base,
            base_text: format!("0x{:X}", m.base),
            size_text: if m.size == 0 {
                String::new()
            } else {
                format!("0x{:X}", m.size)
            },
        }
    }
}

/// Map a list of provider module entries into the display rows, sorted by base
/// address (the C++ module list's natural order). Pure + unit-tested.
pub fn build_module_rows(entries: &[ModuleEntry]) -> Vec<ModuleRow> {
    let mut rows: Vec<ModuleRow> = entries.iter().map(ModuleRow::from_entry).collect();
    rows.sort_by_key(|r| r.base);
    rows
}

// ── gpui view ───────────────────────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use view::ModulesPanel;

#[cfg(feature = "ui")]
mod view {
    use std::sync::Arc;

    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::dock::{Panel, PanelEvent};

    use super::{build_module_rows, ModuleRow, ModulesTab};
    use crate::provider::Provider;
    use crate::ui::design::{color, icon, tokens};

    /// The Modules right-dock panel — a Modules / Symbols / Types tab strip with
    /// a "Download All" action over the active source's module list.
    ///
    /// Owns the selected [`ModulesTab`] and the currently-attached
    /// [`Provider`] (the active tab's source — set via
    /// [`set_provider`](ModulesPanel::set_provider)); the module rows are derived
    /// from the provider on render. Mirrors [`WorkspacePanel`] /
    /// [`ScannerPanel`]'s `view(window, cx) -> Entity<Self>` + `impl Panel`
    /// structure.
    pub struct ModulesPanel {
        tab: ModulesTab,
        provider: Option<Arc<dyn Provider + Send + Sync>>,
        focus_handle: FocusHandle,
    }

    impl ModulesPanel {
        /// Build an empty modules panel (Modules tab selected, no source).
        pub fn new(cx: &mut Context<Self>) -> Self {
            ModulesPanel {
                tab: ModulesTab::Modules,
                provider: None,
                focus_handle: cx.focus_handle(),
            }
        }

        /// Construct as an [`Entity`] (the form a dock holds). Takes `window` to
        /// match the other panels' `view(window, cx)` shape (the docks builder
        /// calls every panel the same way).
        pub fn view(_window: &mut Window, cx: &mut App) -> Entity<Self> {
            cx.new(ModulesPanel::new)
        }

        /// Attach the active tab's [`Provider`] (the C++ source getter result);
        /// the module list reads from this source.
        pub fn set_provider(&mut self, provider: Option<Arc<dyn Provider + Send + Sync>>) {
            self.provider = provider;
        }

        /// The currently-selected tab (for tests / external wiring).
        pub fn tab(&self) -> ModulesTab {
            self.tab
        }

        /// Switch the active tab (the strip click handler).
        fn set_tab(&mut self, tab: ModulesTab, cx: &mut Context<Self>) {
            self.tab = tab;
            cx.notify();
        }

        /// The module rows for the active source (empty without a provider, or
        /// for the Symbols / Types tabs which aren't enumerated here yet).
        fn rows(&self) -> Vec<ModuleRow> {
            if self.tab != ModulesTab::Modules {
                return Vec::new();
            }
            match &self.provider {
                Some(p) => build_module_rows(&p.enumerate_modules()),
                None => Vec::new(),
            }
        }
    }

    impl Panel for ModulesPanel {
        fn panel_name(&self) -> &'static str {
            super::PANEL_NAME
        }

        fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            SharedString::from("Modules")
        }
    }

    impl EventEmitter<PanelEvent> for ModulesPanel {}

    impl Focusable for ModulesPanel {
        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.focus_handle.clone()
        }
    }

    impl Render for ModulesPanel {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let rows = self.rows();
            let cur = self.tab;

            gpui_component::v_flex()
                .id("rcx-modules-panel")
                .track_focus(&self.focus_handle)
                .size_full()
                .bg(color::panel_bg(cx))
                .text_color(color::text(cx))
                .child(self.render_header(cur, cx))
                .child(div().flex_1().min_h_0().child(if rows.is_empty() {
                    empty_state(cur.empty_caption(), cx).into_any_element()
                } else {
                    module_list(rows, cx).into_any_element()
                }))
        }
    }

    impl ModulesPanel {
        /// The Zed panel header: the Modules / Symbols / Types tab strip on the
        /// left, a "Download All" button on the right (the `data_options.png`
        /// top-right cluster).
        fn render_header(&self, cur: ModulesTab, cx: &Context<Self>) -> impl IntoElement {
            let view = cx.entity();

            gpui_component::h_flex()
                .h(px(32.0))
                .w_full()
                .flex_none()
                .px(px(tokens::space::LG))
                .items_center()
                .justify_between()
                .border_b_1()
                .border_color(color::border(cx))
                // ── Tab strip (Modules / Symbols / Types) ──
                .child(
                    gpui_component::h_flex()
                        .gap(px(tokens::space::XS))
                        .items_center()
                        .children(ModulesTab::all().into_iter().map(|tab| {
                            let view = view.clone();
                            tab_chip(tab, tab == cur, cx).on_click(move |_e, _w, cx| {
                                view.update(cx, |this, cx| this.set_tab(tab, cx));
                            })
                        })),
                )
                // ── Download All action ──
                .child(download_all_button(cx))
        }
    }

    /// A single tab-strip chip (Modules / Symbols / Types): a small inset row
    /// that carries the soft-accent fill when active and a hover overlay
    /// otherwise (Zed segmented-tab styling).
    fn tab_chip(tab: ModulesTab, active: bool, cx: &App) -> Stateful<Div> {
        gpui_component::h_flex()
            .id(SharedString::from(format!(
                "rcx-modules-tab-{}",
                tab.label()
            )))
            .h(px(22.0))
            .px(px(tokens::space::MD))
            .items_center()
            .rounded(px(tokens::radius::MD))
            .text_size(px(tokens::font::UI_SM))
            .when(active, |c| {
                c.bg(color::selected_bg(cx)).text_color(color::text(cx))
            })
            .when(!active, |c| {
                c.text_color(color::text_muted(cx))
                    .hover(|s| s.bg(color::hover_overlay(cx)).text_color(color::text(cx)))
            })
            .child(tab.label())
    }

    /// The "Download All" header action (the `data_options.png` top-right
    /// button): a compact ghost button with a leading download/source glyph. It
    /// is a restrained chrome affordance here — the symbol-download workflow is
    /// out of this surface's scope — so it shows the hint state but performs no
    /// mutation yet.
    fn download_all_button(cx: &App) -> impl IntoElement {
        gpui_component::h_flex()
            .id("rcx-modules-download-all")
            .flex_none()
            .h(px(22.0))
            .px(px(tokens::space::MD))
            .gap(px(tokens::space::XS))
            .items_center()
            .rounded(px(tokens::radius::MD))
            .text_size(px(tokens::font::UI_SM))
            .text_color(color::text_muted(cx))
            .hover(|s| s.bg(color::hover_overlay(cx)).text_color(color::text(cx)))
            .child(icon::source().with_size(px(12.0)))
            .child("Download All")
    }

    /// The module list — one row per loaded module (name + base + size). Not
    /// virtualized: a source's module count is small enough to render directly.
    fn module_list(rows: Vec<ModuleRow>, cx: &App) -> impl IntoElement {
        gpui_component::v_flex()
            .id("rcx-modules-list")
            .size_full()
            .px(px(tokens::space::SM))
            .py(px(tokens::space::XS))
            .overflow_y_scroll()
            .children(
                rows.into_iter()
                    .enumerate()
                    .map(|(ix, row)| module_row(ix, row, cx)),
            )
    }

    /// One module row: a leading source glyph, the truncating module name, and a
    /// trailing monospace `0x...` base (the C++ module-list row).
    fn module_row(ix: usize, row: ModuleRow, cx: &App) -> impl IntoElement {
        gpui_component::h_flex()
            .id(("rcx-module-row", ix))
            .w_full()
            .h(px(24.0))
            .px(px(tokens::space::MD))
            .gap(px(tokens::space::MD))
            .items_center()
            .rounded(px(tokens::radius::MD))
            .hover(|s| s.bg(color::hover_overlay(cx)))
            .child(
                icon::source()
                    .with_size(px(12.0))
                    .text_color(color::text_muted(cx)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(tokens::font::UI_MD))
                    .text_color(color::text(cx))
                    .child(SharedString::from(row.name)),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(tokens::font::MONO_FAMILY)
                    .text_size(px(tokens::font::UI_XS))
                    .text_color(color::syntax_address(cx))
                    .child(SharedString::from(row.base_text)),
            )
    }

    /// The clean empty-state body: a centered muted caption (the Zed "nothing
    /// here yet" panel state).
    fn empty_state(caption: &'static str, cx: &App) -> impl IntoElement {
        gpui_component::v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(px(tokens::space::MD))
            .child(
                icon::database()
                    .with_size(px(20.0))
                    .text_color(color::text_disabled(cx)),
            )
            .child(
                div()
                    .text_size(px(tokens::font::UI_SM))
                    .text_color(color::text_muted(cx))
                    .child(caption),
            )
    }

    // Bring the `with_size` (Sizable) + Disableable helpers into scope.
    use gpui_component::Sizable as _;
}

#[cfg(test)]
mod tests {
    use super::{build_module_rows, ModuleRow, ModulesTab};
    use crate::provider::ModuleEntry;

    #[test]
    fn tabs_are_in_display_order() {
        let tabs = ModulesTab::all();
        assert_eq!(tabs[0], ModulesTab::Modules);
        assert_eq!(tabs[1], ModulesTab::Symbols);
        assert_eq!(tabs[2], ModulesTab::Types);
        assert_eq!(ModulesTab::Modules.label(), "Modules");
        assert_eq!(ModulesTab::Symbols.label(), "Symbols");
        assert_eq!(ModulesTab::Types.label(), "Types");
        // The default tab is the live Modules list.
        assert_eq!(ModulesTab::default(), ModulesTab::Modules);
    }

    #[test]
    fn empty_captions_are_distinct() {
        let m = ModulesTab::Modules.empty_caption();
        let s = ModulesTab::Symbols.empty_caption();
        let t = ModulesTab::Types.empty_caption();
        assert!(m.contains("module"));
        assert_ne!(m, s);
        assert_ne!(s, t);
        assert_ne!(m, t);
    }

    #[test]
    fn module_row_formats_name_base_and_size() {
        let entry = ModuleEntry {
            name: "game.exe".into(),
            full_path: "C:\\games\\game.exe".into(),
            base: 0x0040_0000,
            size: 0x1_0000,
        };
        let row = ModuleRow::from_entry(&entry);
        assert_eq!(row.name, "game.exe");
        assert_eq!(row.base, 0x0040_0000);
        assert_eq!(row.base_text, "0x400000");
        assert_eq!(row.size_text, "0x10000");
    }

    #[test]
    fn module_row_falls_back_to_path_tail_when_unnamed() {
        let entry = ModuleEntry {
            name: String::new(),
            full_path: "/usr/lib/libfoo.so".into(),
            base: 0x1000,
            size: 0,
        };
        let row = ModuleRow::from_entry(&entry);
        // Basename of the path, and an empty size string for a zero size.
        assert_eq!(row.name, "libfoo.so");
        assert_eq!(row.size_text, "");
        // Backslash paths also split to their tail.
        let entry2 = ModuleEntry {
            name: String::new(),
            full_path: "C:\\windows\\system32\\ntdll.dll".into(),
            base: 0x2000,
            size: 0x100,
        };
        assert_eq!(ModuleRow::from_entry(&entry2).name, "ntdll.dll");
    }

    #[test]
    fn build_rows_sorts_by_base_address() {
        let entries = vec![
            ModuleEntry {
                name: "high.dll".into(),
                full_path: String::new(),
                base: 0x3000,
                size: 0x10,
            },
            ModuleEntry {
                name: "low.dll".into(),
                full_path: String::new(),
                base: 0x1000,
                size: 0x10,
            },
            ModuleEntry {
                name: "mid.dll".into(),
                full_path: String::new(),
                base: 0x2000,
                size: 0x10,
            },
        ];
        let rows = build_module_rows(&entries);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["low.dll", "mid.dll", "high.dll"]);
    }

    #[test]
    fn build_rows_empty_for_no_modules() {
        assert!(build_module_rows(&[]).is_empty());
    }
}
