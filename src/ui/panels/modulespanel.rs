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
    /// The count of loaded PDB symbols for this module (0 = none loaded). Drives
    /// the "✓ N syms" indicator. Populated from the global symbol store on
    /// render; the pure builder leaves it 0.
    pub symbol_count: usize,
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
            symbol_count: 0,
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

/// One resolved-symbol row's display strings — a symbol name + its module + the
/// `module+RVA` offset (the C++ Symbols tab). Pure + unit-tested.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolRow {
    /// The owning module's canonical name.
    pub module: String,
    /// The symbol name (e.g. `NtCreateFile`).
    pub name: String,
    /// The RVA within the module.
    pub rva: u32,
    /// The formatted `module+0xRVA` offset string.
    pub offset_text: String,
}

/// Build the Symbols-tab rows from the loaded module symbol sets: every
/// `(name, rva)` pair across the loaded modules, sorted by `(module, rva)`,
/// capped at `cap` so a giant PDB doesn't stall the list. Pure + unit-tested.
pub fn build_symbol_rows(sets: &[(String, Vec<(String, u32)>)], cap: usize) -> Vec<SymbolRow> {
    let mut rows: Vec<SymbolRow> = Vec::new();
    for (module, syms) in sets {
        for (name, rva) in syms {
            rows.push(SymbolRow {
                module: module.clone(),
                name: name.clone(),
                rva: *rva,
                offset_text: format!("{module}+0x{rva:X}"),
            });
        }
    }
    rows.sort_by(|a, b| a.module.cmp(&b.module).then(a.rva.cmp(&b.rva)));
    rows.truncate(cap);
    rows
}

/// One imported-PDB-type row's display strings — the type name + module + a
/// kind tag (struct/class/union/enum) + size (the C++ Types tab). Pure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeRow {
    /// The owning module's canonical name.
    pub module: String,
    /// The type name.
    pub name: String,
    /// The kind tag ("enum" / "union" / "struct").
    pub kind: &'static str,
    /// The formatted size (`0x...` bytes; empty when zero).
    pub size_text: String,
}

/// The kind tag for a PDB type (enum > union > struct, matching the C++ tag).
pub fn pdb_type_kind(is_enum: bool, is_union: bool) -> &'static str {
    if is_enum {
        "enum"
    } else if is_union {
        "union"
    } else {
        "struct"
    }
}

/// The `module+RVA` offset string for a symbol (the C++ Symbols tab offset
/// column). Pure helper so the view never formats inline.
pub fn format_symbol_offset(module: &str, rva: u32) -> String {
    format!("{module}+0x{rva:X}")
}

// ── gpui view ───────────────────────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use view::{ModuleAction, ModulesPanel};

#[cfg(feature = "ui")]
mod view {
    use std::sync::Arc;

    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::dock::{Panel, PanelEvent};

    use super::{
        build_module_rows, build_symbol_rows, pdb_type_kind, ModuleRow, ModulesTab, SymbolRow,
        TypeRow,
    };
    use crate::provider::Provider;
    use crate::rtti::SymbolStore;
    use crate::ui::design::{color, icon, tokens};

    /// The max symbol rows the Symbols tab lists (a full ntdll PDB has tens of
    /// thousands; cap so the non-virtualized list stays responsive).
    const SYMBOL_ROW_CAP: usize = 2000;

    /// An intent the modules panel raises for the window to resolve onto the
    /// active document / controller (the read-only-surface pattern). The panel
    /// does not own the document base or the PDB loader, so it emits requests.
    #[derive(Clone, Debug)]
    pub enum ModuleAction {
        /// Double-click a module row: set the active document's base address to
        /// the module base + kick off its PDB symbol load (the C++ module-row
        /// activation). Carries the module base + name.
        Activate { base: u64, name: String },
        /// "Download All" header action: load/download PDB symbols for every
        /// module of the active source (the C++ `download_all`).
        DownloadAll,
    }

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

        /// The module rows for the active source (empty without a provider).
        /// Each row carries a symbol-loaded indicator computed from the global
        /// [`SymbolStore`] (the C++ "✓ N syms" badge).
        fn rows(&self) -> Vec<ModuleRow> {
            let mut rows = match &self.provider {
                Some(p) => build_module_rows(&p.enumerate_modules()),
                None => Vec::new(),
            };
            if let Ok(store) = SymbolStore::global().lock() {
                for r in &mut rows {
                    if let Some(set) = store.module_data(&r.name) {
                        r.symbol_count = set.name_to_rva.len();
                    }
                }
            }
            rows
        }

        /// The resolved-symbol rows for the Symbols tab — every `(name, rva)` of
        /// every loaded module in the global [`SymbolStore`] (the C++ Symbols
        /// tab). Empty until a module's PDB is loaded.
        fn symbol_rows(&self) -> Vec<SymbolRow> {
            let store = match SymbolStore::global().lock() {
                Ok(s) => s,
                Err(_) => return Vec::new(),
            };
            let sets: Vec<(String, Vec<(String, u32)>)> = store
                .loaded_modules()
                .into_iter()
                .filter_map(|m| {
                    store.module_data(&m).map(|set| {
                        let syms: Vec<(String, u32)> = set
                            .name_to_rva
                            .iter()
                            .map(|(n, rva)| (n.clone(), *rva))
                            .collect();
                        (m, syms)
                    })
                })
                .collect();
            build_symbol_rows(&sets, SYMBOL_ROW_CAP)
        }

        /// The imported-PDB-type rows for the Types tab — every TPI type of every
        /// loaded module in the global [`SymbolStore`] (the C++ Types tab).
        fn type_rows(&self) -> Vec<TypeRow> {
            let store = match SymbolStore::global().lock() {
                Ok(s) => s,
                Err(_) => return Vec::new(),
            };
            let mut rows: Vec<TypeRow> = Vec::new();
            for m in store.loaded_modules() {
                if let Some(set) = store.module_data(&m) {
                    for t in &set.types {
                        rows.push(TypeRow {
                            module: m.clone(),
                            name: t.name.clone(),
                            kind: pdb_type_kind(t.is_enum, t.is_union),
                            size_text: if t.size == 0 {
                                String::new()
                            } else {
                                format!("0x{:X}", t.size)
                            },
                        });
                    }
                }
            }
            rows.sort_by(|a, b| a.module.cmp(&b.module).then(a.name.cmp(&b.name)));
            rows
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
    impl EventEmitter<ModuleAction> for ModulesPanel {}

    impl Focusable for ModulesPanel {
        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.focus_handle.clone()
        }
    }

    impl Render for ModulesPanel {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let cur = self.tab;
            let view = cx.entity();

            // Resolve the body per active tab (the C++ Modules / Symbols / Types).
            let body: AnyElement = match cur {
                ModulesTab::Modules => {
                    let rows = self.rows();
                    if rows.is_empty() {
                        empty_state(cur.empty_caption(), cx).into_any_element()
                    } else {
                        module_list(&view, rows, cx).into_any_element()
                    }
                }
                ModulesTab::Symbols => {
                    let rows = self.symbol_rows();
                    if rows.is_empty() {
                        empty_state(cur.empty_caption(), cx).into_any_element()
                    } else {
                        symbol_list(rows, cx).into_any_element()
                    }
                }
                ModulesTab::Types => {
                    let rows = self.type_rows();
                    if rows.is_empty() {
                        empty_state(cur.empty_caption(), cx).into_any_element()
                    } else {
                        type_list(rows, cx).into_any_element()
                    }
                }
            };

            gpui_component::v_flex()
                .id("rcx-modules-panel")
                .track_focus(&self.focus_handle)
                .size_full()
                .bg(color::panel_bg(cx))
                .text_color(color::text(cx))
                .child(self.render_header(cur, cx))
                .child(div().flex_1().min_h_0().child(body))
        }
    }

    impl ModulesPanel {
        /// The Zed panel header: the Modules / Symbols / Types tab strip on the
        /// left, a "Download All" button on the right (the `data_options.png`
        /// top-right cluster).
        fn render_header(&self, cur: ModulesTab, cx: &Context<Self>) -> impl IntoElement {
            let view = cx.entity();

            crate::ui::design::panel_header_strip(cx)
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
                .child({
                    let view = cx.entity();
                    download_all_button(cx).on_click(move |_e, _w, cx| {
                        view.update(cx, |_this, cx| cx.emit(ModuleAction::DownloadAll));
                    })
                })
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
    /// button): a compact ghost button with a leading download/source glyph.
    /// Clicking emits [`ModuleAction::DownloadAll`] for the window to resolve
    /// against the active source's PDB loader.
    fn download_all_button(cx: &App) -> Stateful<Div> {
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

    /// The module list — one row per loaded module (name + size + base +
    /// symbol-loaded indicator). Not virtualized: a source's module count is
    /// small enough to render directly. Each row double-clicks to activate
    /// (set base + load PDB).
    fn module_list(
        view: &Entity<ModulesPanel>,
        rows: Vec<ModuleRow>,
        cx: &App,
    ) -> impl IntoElement {
        crate::ui::design::panel_list("rcx-modules-list").children(
            rows.into_iter()
                .enumerate()
                .map(|(ix, row)| module_row(view, ix, row, cx)),
        )
    }

    /// One module row: a leading source glyph, the truncating module name, a
    /// `✓ N syms` symbol-loaded indicator (when symbols are loaded), and a
    /// trailing monospace size + `0x...` base (the C++ module-list row).
    /// Double-clicking emits [`ModuleAction::Activate`].
    fn module_row(
        view: &Entity<ModulesPanel>,
        ix: usize,
        row: ModuleRow,
        cx: &App,
    ) -> impl IntoElement {
        use gpui_component::ActiveTheme as _;
        let act_view = view.clone();
        let base = row.base;
        let name = row.name.clone();
        gpui_component::h_flex()
            .id(("rcx-module-row", ix))
            .w_full()
            .h(px(24.0))
            .px(px(tokens::space::MD))
            .gap(px(tokens::space::MD))
            .items_center()
            .rounded(px(tokens::radius::MD))
            .hover(|s| s.bg(color::hover_overlay(cx)))
            .on_click(move |e: &gpui::ClickEvent, _w, cx| {
                // Double-click activates: set base + load PDB (the C++ row
                // activation). A single click is a no-op selection.
                if e.click_count() >= 2 {
                    let name = name.clone();
                    act_view.update(cx, |_this, cx| {
                        cx.emit(ModuleAction::Activate { base, name });
                    });
                }
            })
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
            // ✓ N syms indicator (the C++ symbol-loaded badge).
            .when(row.symbol_count > 0, |r| {
                r.child(
                    div()
                        .flex_none()
                        .text_size(px(tokens::font::UI_XS))
                        .text_color(cx.theme().green)
                        .child(SharedString::from(format!("✓ {} syms", row.symbol_count))),
                )
            })
            // Size column (the C++ ModuleRow.size_text — was dropped).
            .when(!row.size_text.is_empty(), |r| {
                r.child(
                    div()
                        .flex_none()
                        .font_family(tokens::font::mono_family())
                        .text_size(px(tokens::font::UI_XS))
                        .text_color(color::text_muted(cx))
                        .child(SharedString::from(row.size_text)),
                )
            })
            .child(
                div()
                    .flex_none()
                    .font_family(tokens::font::mono_family())
                    .text_size(px(tokens::font::UI_XS))
                    .text_color(color::syntax_address(cx))
                    .child(SharedString::from(row.base_text)),
            )
    }

    /// The Symbols list — one row per resolved symbol (name + `module+RVA`).
    fn symbol_list(rows: Vec<SymbolRow>, cx: &App) -> impl IntoElement {
        crate::ui::design::panel_list("rcx-symbols-list").children(
            rows.into_iter().enumerate().map(|(ix, row)| {
                gpui_component::h_flex()
                    .id(("rcx-symbol-row", ix))
                    .w_full()
                    .h(px(22.0))
                    .px(px(tokens::space::MD))
                    .gap(px(tokens::space::MD))
                    .items_center()
                    .rounded(px(tokens::radius::MD))
                    .hover(|s| s.bg(color::hover_overlay(cx)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(tokens::font::UI_SM))
                            .text_color(color::text(cx))
                            .child(SharedString::from(row.name)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_family(tokens::font::mono_family())
                            .text_size(px(tokens::font::UI_XS))
                            .text_color(color::syntax_address(cx))
                            .child(SharedString::from(row.offset_text)),
                    )
            }),
        )
    }

    /// The Types list — one row per imported PDB type (kind tag + name + size).
    fn type_list(rows: Vec<TypeRow>, cx: &App) -> impl IntoElement {
        crate::ui::design::panel_list("rcx-types-list").children(rows.into_iter().enumerate().map(
            |(ix, row)| {
                gpui_component::h_flex()
                    .id(("rcx-type-row", ix))
                    .w_full()
                    .h(px(22.0))
                    .px(px(tokens::space::MD))
                    .gap(px(tokens::space::MD))
                    .items_center()
                    .rounded(px(tokens::radius::MD))
                    .hover(|s| s.bg(color::hover_overlay(cx)))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(tokens::font::UI_XS))
                            .text_color(color::text_muted(cx))
                            .child(row.kind),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(tokens::font::UI_SM))
                            .text_color(color::text(cx))
                            .child(SharedString::from(row.name)),
                    )
                    .when(!row.size_text.is_empty(), |r| {
                        r.child(
                            div()
                                .flex_none()
                                .font_family(tokens::font::mono_family())
                                .text_size(px(tokens::font::UI_XS))
                                .text_color(color::text_muted(cx))
                                .child(SharedString::from(row.size_text)),
                        )
                    })
            },
        ))
    }

    /// The clean empty-state body: a centered muted caption (the Zed "nothing
    /// here yet" panel state).
    fn empty_state(caption: &'static str, cx: &App) -> impl IntoElement {
        crate::ui::design::empty_state(icon::database(), caption, cx)
    }

    // Bring the `with_size` (Sizable) + Disableable helpers into scope.
    use gpui_component::Sizable as _;
}

#[cfg(test)]
mod tests {
    use super::{
        build_module_rows, build_symbol_rows, format_symbol_offset, pdb_type_kind, ModuleRow,
        ModulesTab,
    };
    use crate::provider::ModuleEntry;

    #[test]
    fn symbol_rows_sorted_by_module_then_rva_and_capped() {
        let sets = vec![
            (
                "game".to_string(),
                vec![("zzz".to_string(), 0x200u32), ("aaa".to_string(), 0x10u32)],
            ),
            ("ntdll".to_string(), vec![("Nt".to_string(), 0x5u32)]),
        ];
        let rows = build_symbol_rows(&sets, 100);
        // game's rows come first (module sort), ordered by rva ascending.
        assert_eq!(rows[0].module, "game");
        assert_eq!(rows[0].name, "aaa");
        assert_eq!(rows[0].rva, 0x10);
        assert_eq!(rows[0].offset_text, "game+0x10");
        assert_eq!(rows[1].name, "zzz");
        assert_eq!(rows[2].module, "ntdll");
        // Cap truncates.
        let capped = build_symbol_rows(&sets, 1);
        assert_eq!(capped.len(), 1);
    }

    #[test]
    fn pdb_type_kind_precedence() {
        assert_eq!(pdb_type_kind(true, false), "enum");
        assert_eq!(pdb_type_kind(true, true), "enum"); // enum wins
        assert_eq!(pdb_type_kind(false, true), "union");
        assert_eq!(pdb_type_kind(false, false), "struct");
    }

    #[test]
    fn symbol_offset_is_module_plus_hex_rva() {
        assert_eq!(format_symbol_offset("ntdll", 0x1234), "ntdll+0x1234");
        assert_eq!(format_symbol_offset("game.exe", 0), "game.exe+0x0");
    }

    #[test]
    fn module_row_defaults_symbol_count_to_zero() {
        let entry = ModuleEntry {
            name: "x.dll".into(),
            full_path: String::new(),
            base: 0x1000,
            size: 0x10,
        };
        assert_eq!(ModuleRow::from_entry(&entry).symbol_count, 0);
    }

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
