//! Command palette — fuzzy-search every menu command and trigger it
//! (`commandpalette.h`, widgets-dialogs.md §8).
//!
//! Port of `CommandPalette`: a centered modal that walks the menu bar into flat
//! "Menu > Sub > Item" [`Entry`] paths, fuzzy-filters them with the palette's own
//! scorer ([`super::fuzzy::command_score`]), stable-sorts by score, and triggers
//! the chosen entry's command. The C++ reimplements no behavior — it surfaces
//! already-wired `QAction`s; here an entry carries a [`CommandId`] the host routes
//! to the controller/window (Ctrl+K opens it; ARCHITECTURE §5 → `List` modal).
//!
//! Split (gpui-free model + thin view):
//! - [`MenuNode`] / [`default_menu_tree`] — the Reclass menu bar as data (the
//!   File/Edit/View/Tools/Plugins/Help tree with shortcuts; app-shell §7), the
//!   port's equivalent of the live `QMenuBar`.
//! - [`Entry`] / [`walk_menu`] — the recursive, cycle-safe flattener (`walkMenu`):
//!   strips separators + `&` mnemonics + `\t` shortcut hints; leaf → an entry.
//! - [`PaletteModel`] — the filter/rank state (`rebuildModel`): score via
//!   [`command_score`](super::fuzzy::command_score), keep `>0`, **stable-sort by
//!   score desc**, build "path    [shortcut]" rows; selects row 0.
//! - [`CommandPalette`] / [`PaletteEvent`] — the gpui modal raising
//!   `Trigger(CommandId)` / `Cancel`.
//!
//! Gated behind the `ui` feature.

use super::fuzzy::command_score;

/// A routed command identifier — the port's stand-in for a `QAction*` target.
/// The host maps these to controller/window operations (the palette itself only
/// surfaces and triggers them).
pub type CommandId = String;

/// A node in the menu tree — either a submenu (label + children) or a leaf
/// command, or a separator. Mirrors a `QMenu` / `QAction` (`walkMenu` input).
#[derive(Clone, Debug)]
pub enum MenuNode {
    /// A submenu: a label and its children.
    Submenu {
        label: String,
        children: Vec<MenuNode>,
    },
    /// A leaf command: visible label, optional shortcut text, command id, enabled.
    Item {
        label: String,
        shortcut: String,
        command: CommandId,
        enabled: bool,
    },
    /// A separator (skipped by [`walk_menu`]).
    Separator,
}

impl MenuNode {
    /// Build an enabled leaf command.
    pub fn item(label: &str, shortcut: &str, command: &str) -> MenuNode {
        MenuNode::Item {
            label: label.to_string(),
            shortcut: shortcut.to_string(),
            command: command.to_string(),
            enabled: true,
        }
    }

    /// Build a disabled leaf command (`a->isEnabled() == false`).
    pub fn disabled_item(label: &str, shortcut: &str, command: &str) -> MenuNode {
        MenuNode::Item {
            label: label.to_string(),
            shortcut: shortcut.to_string(),
            command: command.to_string(),
            enabled: false,
        }
    }

    /// Build a submenu.
    pub fn submenu(label: &str, children: Vec<MenuNode>) -> MenuNode {
        MenuNode::Submenu {
            label: label.to_string(),
            children,
        }
    }
}

/// A flattened command entry (`struct Entry`, `commandpalette.h:30`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    /// "File > Recent Files > foo.rcx" (mnemonics + shortcut hints stripped).
    pub path: String,
    /// "Ctrl+O" (native text), empty if none.
    pub shortcut: String,
    /// The command to trigger.
    pub command: CommandId,
    /// Whether the action is enabled (disabled ones show but aren't selectable).
    pub enabled: bool,
}

/// Strip a `\t`-embedded shortcut hint and `&` mnemonics from a menu label
/// (`walkMenu`: `left(tabPos)` then `remove('&')`).
fn clean_label(label: &str) -> String {
    let base = match label.find('\t') {
        Some(pos) => &label[..pos],
        None => label,
    };
    base.replace('&', "")
}

/// Recursively flatten a menu tree into `entries` (`walkMenu`,
/// `commandpalette.h:204`). Skips separators + blank labels; submenus recurse
/// with `path + " > " + label`; leaves push an [`Entry`]. (Our tree is acyclic by
/// construction, so the C++ `QSet<QMenu*> seen` cycle guard is unnecessary, but
/// the traversal order is identical.)
pub fn walk_menu(node: &MenuNode, path: &str, entries: &mut Vec<Entry>) {
    match node {
        MenuNode::Separator => {}
        MenuNode::Submenu { label, children } => {
            let clean = clean_label(label);
            if clean.trim().is_empty() {
                return;
            }
            let sub_path = if path.is_empty() {
                clean
            } else {
                format!("{path} > {clean}")
            };
            for child in children {
                walk_menu(child, &sub_path, entries);
            }
        }
        MenuNode::Item {
            label,
            shortcut,
            command,
            enabled,
        } => {
            let clean = clean_label(label);
            if clean.trim().is_empty() {
                return;
            }
            let full_path = if path.is_empty() {
                clean
            } else {
                format!("{path} > {clean}")
            };
            entries.push(Entry {
                path: full_path,
                shortcut: shortcut.clone(),
                command: command.clone(),
                enabled: *enabled,
            });
        }
    }
}

/// Flatten a whole menu bar (`populateFromMenuBar`): walk each top-level menu,
/// seeding `path` with the top menu's (cleaned) title.
pub fn flatten_menu_bar(top_level: &[MenuNode]) -> Vec<Entry> {
    let mut entries = Vec::new();
    for node in top_level {
        // Top-level entries are menus; their title seeds the path.
        if let MenuNode::Submenu { label, children } = node {
            let title = clean_label(label);
            if title.trim().is_empty() {
                continue;
            }
            for child in children {
                walk_menu(child, &title, &mut entries);
            }
        } else {
            walk_menu(node, "", &mut entries);
        }
    }
    entries
}

/// The Reclass menu bar as data (app-shell §7 `createMenus`) — the full command
/// set + shortcuts the palette searches. Faithful to the C++ menu tree (the
/// exact paths/shortcuts drive parity).
pub fn default_menu_tree() -> Vec<MenuNode> {
    use MenuNode as N;
    vec![
        N::submenu(
            "&File",
            vec![
                N::item("New Class", "Ctrl+N", "file.new_class"),
                N::item("New Struct", "Ctrl+T", "file.new_struct"),
                N::item("New Enum", "Ctrl+E", "file.new_enum"),
                N::item("Open…", "Ctrl+O", "file.open"),
                N::submenu("Recent Files", vec![]),
                N::item("Welcome Screen", "", "file.welcome"),
                N::Separator,
                N::item("Save", "Ctrl+S", "file.save"),
                N::item("Save As…", "Ctrl+Shift+S", "file.save_as"),
                N::Separator,
                N::submenu(
                    "Import",
                    vec![
                        N::item("From Source…", "", "import.source"),
                        N::item("ReClass XML…", "", "import.xml"),
                        N::item("PDB…", "", "import.pdb"),
                    ],
                ),
                N::submenu(
                    "Export",
                    vec![
                        N::item("C++ Header…", "", "export.cpp"),
                        N::item("Rust Structs…", "", "export.rust"),
                        N::item("#define Offsets…", "", "export.defines"),
                        N::item("C# Structs…", "", "export.csharp"),
                        N::item("Python ctypes…", "", "export.python"),
                        N::item("ReClass XML…", "", "export.xml"),
                    ],
                ),
                N::Separator,
                N::item("Close Project", "Ctrl+W", "file.close"),
                N::Separator,
                N::item("Exit", "", "file.exit"),
            ],
        ),
        N::submenu(
            "&Edit",
            vec![
                N::item("Undo", "Ctrl+Z", "edit.undo"),
                N::item("Redo", "Ctrl+Y", "edit.redo"),
                N::Separator,
                N::item("Find Field…", "Ctrl+F", "edit.find"),
                N::Separator,
                N::item("Add Bookmark…", "Ctrl+B", "edit.add_bookmark"),
                N::item("Quick Bookmark Here", "Ctrl+Alt+B", "edit.quick_bookmark"),
            ],
        ),
        N::submenu(
            "&View",
            vec![
                N::item("Reset Windows", "", "view.reset_windows"),
                N::Separator,
                N::item("Compact Columns", "", "view.compact_columns"),
                N::item("Tree Lines", "", "view.tree_lines"),
                N::item("Relative Offsets", "", "view.relative_offsets"),
                N::Separator,
                N::item("Show Comment chips", "", "view.comment_chips"),
                N::item("Show RTTI chips", "", "view.rtti_chips"),
                N::item("Show Enum-value chips", "", "view.enum_chips"),
                N::Separator,
                N::item("Refresh", "F5", "view.refresh"),
                N::item("Go to Address…", "Ctrl+G", "view.goto_address"),
                N::item("Command Palette…", "Ctrl+K", "view.command_palette"),
                N::Separator,
                N::item("Split View Below", "Ctrl+\\", "view.split"),
                N::item("Unsplit View", "Ctrl+Shift+\\", "view.unsplit"),
                N::Separator,
                N::item("Memory Scanner", "Ctrl+Shift+S", "view.scanner"),
                N::item("Symbols", "Ctrl+Shift+Y", "view.symbols"),
                N::item("Bookmarks", "Ctrl+Shift+B", "view.bookmarks"),
                N::Separator,
                N::item("Presentation Mode", "Ctrl+Shift+P", "view.presentation"),
            ],
        ),
        N::submenu(
            "&Tools",
            vec![
                N::item("RTTI Browser", "Ctrl+Shift+R", "tools.rtti"),
                N::item("Type Aliases…", "", "tools.type_aliases"),
                N::item("Validate Project…", "Ctrl+Shift+V", "tools.validate"),
                N::item("Performance Profiler…", "Ctrl+Shift+F", "tools.profiler"),
                N::Separator,
                N::item("Start/Stop MCP Server", "", "tools.mcp"),
                N::Separator,
                N::item("Options…", "", "tools.options"),
            ],
        ),
        N::submenu(
            "&Plugins",
            vec![N::item("Manage Plugins…", "", "plugins.manage")],
        ),
        N::submenu(
            "&Help",
            vec![
                N::item("Keyboard Shortcuts…", "F1", "help.shortcuts"),
                N::item("About Reclass", "", "help.about"),
            ],
        ),
    ]
}

/// A scored, filtered, sorted palette model (`rebuildModel`,
/// `commandpalette.h:231`).
#[derive(Clone, Debug, Default)]
pub struct PaletteModel {
    entries: Vec<Entry>,
    /// The filtered + ranked entry indices (into `entries`), best score first.
    ranked: Vec<usize>,
    /// The selected row (index into `ranked`); `None` when empty.
    selected: Option<usize>,
}

impl PaletteModel {
    /// Build a model over the flattened menu-bar entries, unfiltered.
    pub fn new(entries: Vec<Entry>) -> Self {
        let mut m = PaletteModel {
            entries,
            ranked: Vec::new(),
            selected: None,
        };
        m.apply_filter("");
        m
    }

    /// All flattened entries (test accessor `entries()`).
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The ranked entries (filtered + sorted), in display order.
    pub fn ranked(&self) -> Vec<&Entry> {
        self.ranked.iter().map(|&i| &self.entries[i]).collect()
    }

    /// The number of visible rows.
    pub fn row_count(&self) -> usize {
        self.ranked.len()
    }

    /// The selected ranked row.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// The entry under the selected row, if any.
    pub fn selected_entry(&self) -> Option<&Entry> {
        self.selected
            .and_then(|r| self.ranked.get(r))
            .map(|&i| &self.entries[i])
    }

    /// The display label for a ranked row: "path    [shortcut]"
    /// (`rebuildModel`). Disabled entries are still shown (greyed by the view).
    pub fn label_at(&self, row: usize) -> Option<String> {
        let e = self.ranked.get(row).map(|&i| &self.entries[i])?;
        Some(if e.shortcut.is_empty() {
            e.path.clone()
        } else {
            format!("{}    [{}]", e.path, e.shortcut)
        })
    }

    /// Re-filter + re-rank against `filter` (`rebuildModel`): score every entry,
    /// keep `>0`, **stable-sort by score desc**, select row 0 if any. The trimmed
    /// filter mirrors `applyFilter` (`commandpalette.h:183`).
    pub fn apply_filter(&mut self, filter: &str) {
        let trimmed = filter.trim();
        let mut scored: Vec<(i32, usize)> = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            let s = command_score(trimmed, &e.path);
            if s > 0 {
                scored.push((s, i));
            }
        }
        // stable_sort by score desc (preserves enumeration order for ties).
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        self.ranked = scored.into_iter().map(|(_, i)| i).collect();
        self.selected = if self.ranked.is_empty() {
            None
        } else {
            Some(0)
        };
    }

    /// Move the selection down (Down/PageDown), skipping disabled rows is NOT
    /// done by the C++ palette (disabled rows are non-selectable in the model but
    /// arrow-nav still lands on the next row); we mirror the simple clamp move.
    pub fn move_down(&mut self) {
        if let Some(r) = self.selected {
            if r + 1 < self.ranked.len() {
                self.selected = Some(r + 1);
            }
        } else if !self.ranked.is_empty() {
            self.selected = Some(0);
        }
    }

    /// Move the selection up (Up/PageUp).
    pub fn move_up(&mut self) {
        if let Some(r) = self.selected {
            if r > 0 {
                self.selected = Some(r - 1);
            }
        }
    }

    /// Activate a ranked row (`activateEntry`): returns the command id iff the row
    /// exists and its entry is enabled (`bounds + enabled` check). Disabled / OOB
    /// → `None`.
    pub fn activate(&self, row: usize) -> Option<CommandId> {
        let &idx = self.ranked.get(row)?;
        let e = &self.entries[idx];
        if !e.enabled {
            return None;
        }
        Some(e.command.clone())
    }

    /// Activate the current selection, else row 0 (`activateCurrent`).
    pub fn activate_current(&self) -> Option<CommandId> {
        let row = self.selected.or(if self.ranked.is_empty() {
            None
        } else {
            Some(0)
        })?;
        self.activate(row)
    }
}

// ── gpui view ───────────────────────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use view::{command_palette_key_bindings, CommandPalette, PaletteEvent};

#[cfg(feature = "ui")]
mod view {
    use super::{default_menu_tree, flatten_menu_bar, CommandId, PaletteModel};
    use crate::ui::design::{color, tokens};
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::input::{Input, InputEvent, InputState};

    /// A Zed-style key-cap chip (`SM` radius, `UI_XS`, hairline border, muted
    /// text) for a single keystroke token shown in the row's end slot.
    fn key_cap(text: &str, cx: &gpui::App) -> AnyElement {
        div()
            .px(px(tokens::space::SM))
            .h(px(18.))
            .min_w(px(18.))
            .rounded(px(tokens::radius::SM))
            .bg(color::hover_overlay(cx))
            .border_1()
            .border_color(color::border(cx))
            .text_size(px(tokens::font::UI_XS))
            .text_color(color::text_muted(cx))
            .flex()
            .items_center()
            .justify_center()
            .child(text.to_string())
            .into_any_element()
    }

    /// Render a command-path label with the fuzzy-matched characters emphasized
    /// (Zed `command_palette::render_match`: matched chars in the accent color +
    /// semibold, the rest muted). `positions` are char indices into `path`.
    fn highlighted_path(
        path: &str,
        positions: &[usize],
        base: Hsla,
        accent: Hsla,
    ) -> Vec<AnyElement> {
        let pos: std::collections::BTreeSet<usize> = positions.iter().copied().collect();
        // Coalesce runs of (matched | unmatched) chars into spans so we emit few
        // elements; each span carries the matched styling or the base styling.
        let mut spans: Vec<AnyElement> = Vec::new();
        let mut cur = String::new();
        let mut cur_hit: Option<bool> = None;
        let flush = |spans: &mut Vec<AnyElement>, text: &str, hit: bool| {
            if text.is_empty() {
                return;
            }
            let mut el = div().child(text.to_string());
            if hit {
                el = el.text_color(accent).font_weight(FontWeight::SEMIBOLD);
            } else {
                el = el.text_color(base);
            }
            spans.push(el.into_any_element());
        };
        for (i, ch) in path.chars().enumerate() {
            let hit = pos.contains(&i);
            if cur_hit != Some(hit) {
                if let Some(prev) = cur_hit {
                    flush(&mut spans, &cur, prev);
                }
                cur.clear();
                cur_hit = Some(hit);
            }
            cur.push(ch);
        }
        if let Some(prev) = cur_hit {
            flush(&mut spans, &cur, prev);
        }
        spans
    }

    actions!(
        rcx_command_palette,
        [PaletteDown, PaletteUp, PaletteConfirm, PaletteCancel]
    );

    /// The key bindings for the command palette (bound in the `RcxCommandPalette`
    /// context). Returned so the app registers them once at startup.
    pub fn command_palette_key_bindings() -> Vec<KeyBinding> {
        vec![
            KeyBinding::new("down", PaletteDown, Some("RcxCommandPalette")),
            KeyBinding::new("up", PaletteUp, Some("RcxCommandPalette")),
            KeyBinding::new("enter", PaletteConfirm, Some("RcxCommandPalette")),
            KeyBinding::new("escape", PaletteCancel, Some("RcxCommandPalette")),
        ]
    }

    /// The palette's outcome (the C++ `accept`/`reject`).
    #[derive(Clone, Debug)]
    pub enum PaletteEvent {
        /// Trigger the chosen command (`action->trigger()`).
        Trigger(CommandId),
        /// Dismissed (Esc).
        Cancel,
    }

    /// The command-palette modal view.
    pub struct CommandPalette {
        model: PaletteModel,
        input: Entity<InputState>,
        focus_handle: FocusHandle,
        _subscription: Subscription,
    }

    impl CommandPalette {
        /// Build the palette over the default Reclass menu tree.
        pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
            Self::with_entries(flatten_menu_bar(&default_menu_tree()).into(), window, cx)
        }

        /// Build the palette over a specific flattened entry list (for tests /
        /// custom command sets).
        pub fn with_entries(
            entries: Box<[super::Entry]>,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> Self {
            let model = PaletteModel::new(entries.into_vec());
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Type a command..."));
            let subscription =
                cx.subscribe_in(&input, window, |this, _i, ev: &InputEvent, _window, cx| {
                    if matches!(ev, InputEvent::Change) {
                        let q = this.input.read(cx).value().to_string();
                        this.model.apply_filter(&q);
                        cx.notify();
                    }
                });
            CommandPalette {
                model,
                input,
                focus_handle: cx.focus_handle(),
                _subscription: subscription,
            }
        }

        /// Read-only access to the model (tests / wiring).
        pub fn model(&self) -> &PaletteModel {
            &self.model
        }

        fn on_down(&mut self, _: &PaletteDown, _: &mut Window, cx: &mut Context<Self>) {
            self.model.move_down();
            cx.notify();
        }
        fn on_up(&mut self, _: &PaletteUp, _: &mut Window, cx: &mut Context<Self>) {
            self.model.move_up();
            cx.notify();
        }
        fn on_confirm(&mut self, _: &PaletteConfirm, _: &mut Window, cx: &mut Context<Self>) {
            if let Some(cmd) = self.model.activate_current() {
                cx.emit(PaletteEvent::Trigger(cmd));
            }
        }
        fn on_cancel(&mut self, _: &PaletteCancel, _: &mut Window, cx: &mut Context<Self>) {
            cx.emit(PaletteEvent::Cancel);
        }

        /// Click a row → trigger it (the C++ list `clicked`/`activated`).
        fn click_row(&mut self, row: usize, cx: &mut Context<Self>) {
            if let Some(cmd) = self.model.activate(row) {
                cx.emit(PaletteEvent::Trigger(cmd));
            }
        }
    }

    impl Focusable for CommandPalette {
        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.focus_handle.clone()
        }
    }

    impl EventEmitter<PaletteEvent> for CommandPalette {}

    impl Render for CommandPalette {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            // Visual treatment mirrors Zed's command palette (crates/command_palette
            // `render_match`): an elevated rounded modal; each row is an inset item
            // with the command name on the left (matched chars emphasized) and
            // key-cap chips on the right (`justify_between`), a soft-accent fill on
            // the selected row. All geometry/color comes from the shared tokens.
            let selected = self.model.selected();
            let query = self.input.read(cx).value().to_string();
            let query = query.trim().to_string();

            // Per-row fuzzy-highlight positions (visual only — ranking stays the
            // command scorer; positions use the strict matcher over the full path).
            let entries: Vec<(String, String, bool, Vec<usize>)> = self
                .model
                .ranked()
                .iter()
                .map(|e| {
                    let mut pos = Vec::new();
                    if !query.is_empty() {
                        super::super::fuzzy::fuzzy_score(&query, &e.path, Some(&mut pos));
                    }
                    (e.path.clone(), e.shortcut.clone(), e.enabled, pos)
                })
                .collect();

            let fg = color::text(cx);
            let disabled = color::text_disabled(cx);
            let accent = color::accent(cx);
            let border = color::border(cx);
            let sel_bg = color::selected_bg(cx);
            let hover_bg = color::hover_overlay(cx);

            let rows: Vec<AnyElement> = entries
                .into_iter()
                .enumerate()
                .map(|(row, (path, shortcut, enabled, positions))| {
                    let is_sel = selected == Some(row);
                    let name_color = if enabled { fg } else { disabled };
                    let name_spans = highlighted_path(&path, &positions, name_color, accent);
                    let caps: Vec<AnyElement> = if shortcut.is_empty() {
                        Vec::new()
                    } else {
                        shortcut.split('+').map(|k| key_cap(k.trim(), cx)).collect()
                    };
                    div()
                        .id(("palette-row", row))
                        .flex()
                        .flex_row()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap(px(tokens::space::MD))
                        .px(px(tokens::space::MD))
                        .h(px(28.))
                        .rounded(px(tokens::radius::MD))
                        .text_size(px(tokens::font::UI_MD))
                        .when(is_sel, |d| d.bg(sel_bg))
                        .when(!is_sel && enabled, |d| d.hover(|s| s.bg(hover_bg)))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _e, _window, cx| this.click_row(row, cx)))
                        .child(
                            gpui_component::h_flex()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .children(name_spans),
                        )
                        .child(
                            gpui_component::h_flex()
                                .flex_none()
                                .gap(px(tokens::space::XS))
                                .children(caps),
                        )
                        .into_any_element()
                })
                .collect();

            gpui_component::v_flex()
                .id("rcx-command-palette")
                .track_focus(&self.focus_handle)
                .key_context("RcxCommandPalette")
                .on_action(cx.listener(Self::on_down))
                .on_action(cx.listener(Self::on_up))
                .on_action(cx.listener(Self::on_confirm))
                .on_action(cx.listener(Self::on_cancel))
                .w(px(580.))
                .max_h(px(460.))
                .bg(color::elevated_bg(cx))
                .rounded(px(tokens::radius::XL))
                .border_1()
                .border_color(border)
                .shadow_lg()
                .overflow_hidden()
                .child(
                    div()
                        .px(px(tokens::space::LG))
                        .py(px(tokens::space::MD))
                        .border_b_1()
                        .border_color(border)
                        .child(Input::new(&self.input).w_full()),
                )
                .child(
                    gpui_component::v_flex()
                        .id("rcx-command-palette-list")
                        .p(px(tokens::space::XS))
                        .gap(px(tokens::space::XXS))
                        .flex_1()
                        .min_h_0()
                        .overflow_y_hidden()
                        .children(rows),
                )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{default_menu_tree, flatten_menu_bar, walk_menu, MenuNode, PaletteModel};

    fn fixture_bar() -> Vec<MenuNode> {
        use MenuNode as N;
        vec![
            N::submenu(
                "&File",
                vec![
                    N::item("Open", "Ctrl+O", "file.open"),
                    N::Separator,
                    N::item("Save", "Ctrl+S", "file.save"),
                    N::disabled_item("Reopen", "", "file.reopen"),
                ],
            ),
            N::submenu("&Edit", vec![N::item("Undo", "Ctrl+Z", "edit.undo")]),
        ]
    }

    #[test]
    fn flatten_enumerates_leaf_actions_and_skips_separators() {
        let entries = flatten_menu_bar(&fixture_bar());
        // 3 File leaves (Open, Save, Reopen) + 1 Edit leaf = 4 (separator skipped).
        assert_eq!(entries.len(), 4);
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"File > Open"));
        assert!(paths.contains(&"File > Save"));
        assert!(paths.contains(&"Edit > Undo"));
    }

    #[test]
    fn mnemonics_and_shortcut_hints_are_stripped() {
        let mut out = Vec::new();
        walk_menu(
            &MenuNode::item("&Save\tCtrl+S", "Ctrl+S", "file.save"),
            "File",
            &mut out,
        );
        assert_eq!(out.len(), 1);
        // The `\t` hint and `&` mnemonic are removed from the path.
        assert_eq!(out[0].path, "File > Save");
        assert_eq!(out[0].shortcut, "Ctrl+S");
    }

    #[test]
    fn disabled_actions_included_but_flagged() {
        let entries = flatten_menu_bar(&fixture_bar());
        let reopen = entries.iter().find(|e| e.path == "File > Reopen").unwrap();
        assert!(!reopen.enabled);
        let open = entries.iter().find(|e| e.path == "File > Open").unwrap();
        assert!(open.enabled);
    }

    #[test]
    fn shortcuts_captured_nonempty() {
        let entries = flatten_menu_bar(&fixture_bar());
        let open = entries.iter().find(|e| e.path == "File > Open").unwrap();
        assert_eq!(open.shortcut, "Ctrl+O");
    }

    #[test]
    fn empty_filter_matches_all() {
        let model = PaletteModel::new(flatten_menu_bar(&fixture_bar()));
        // Empty needle → every entry matches (score 1).
        assert_eq!(model.row_count(), 4);
        // Row 0 is selected.
        assert_eq!(model.selected(), Some(0));
    }

    #[test]
    fn word_start_outranks_mid_word() {
        let model_entries = flatten_menu_bar(&fixture_bar());
        let mut model = PaletteModel::new(model_entries);
        // "open" matches "File > Open" (word-start) over "File > Reopen" (mid-word).
        model.apply_filter("open");
        let ranked = model.ranked();
        assert!(!ranked.is_empty());
        // The top hit is the word-start "Open".
        assert_eq!(ranked[0].path, "File > Open");
    }

    #[test]
    fn miss_filters_everything_out() {
        let mut model = PaletteModel::new(flatten_menu_bar(&fixture_bar()));
        model.apply_filter("zzzzz");
        assert_eq!(model.row_count(), 0);
        assert_eq!(model.selected(), None);
        assert_eq!(model.activate_current(), None);
    }

    #[test]
    fn activate_disabled_returns_none() {
        let mut model = PaletteModel::new(flatten_menu_bar(&fixture_bar()));
        model.apply_filter("reopen");
        // Reopen is the (only) hit but disabled → not activatable.
        assert_eq!(model.row_count(), 1);
        assert_eq!(model.activate(0), None);
        assert_eq!(model.activate_current(), None);
    }

    #[test]
    fn activate_enabled_returns_command() {
        let mut model = PaletteModel::new(flatten_menu_bar(&fixture_bar()));
        model.apply_filter("undo");
        assert_eq!(model.activate_current().as_deref(), Some("edit.undo"));
    }

    #[test]
    fn label_includes_shortcut_in_brackets() {
        let mut model = PaletteModel::new(flatten_menu_bar(&fixture_bar()));
        model.apply_filter("File > Open");
        let label = model.label_at(0).unwrap();
        assert_eq!(label, "File > Open    [Ctrl+O]");
    }

    #[test]
    fn navigation_clamps_within_bounds() {
        let mut model = PaletteModel::new(flatten_menu_bar(&fixture_bar()));
        assert_eq!(model.selected(), Some(0));
        model.move_up(); // already at 0 → stays
        assert_eq!(model.selected(), Some(0));
        for _ in 0..10 {
            model.move_down();
        }
        // Clamped to last row (3).
        assert_eq!(model.selected(), Some(model.row_count() - 1));
    }

    #[test]
    fn default_menu_tree_has_core_commands() {
        let entries = flatten_menu_bar(&default_menu_tree());
        let cmds: Vec<&str> = entries.iter().map(|e| e.command.as_str()).collect();
        // Spot-check load-bearing commands + the palette/goto entries.
        assert!(cmds.contains(&"file.save"));
        assert!(cmds.contains(&"edit.find"));
        assert!(cmds.contains(&"view.goto_address"));
        assert!(cmds.contains(&"view.command_palette"));
        assert!(cmds.contains(&"tools.options"));
        // Recent Files (empty submenu) contributes no leaf entries.
        assert!(!cmds.iter().any(|c| c.contains("recent")));
    }
}
