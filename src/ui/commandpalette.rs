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

/// The display names of the bundled built-in themes, in built-in order — the
/// gpui-free source for the **View ▸ Theme ▸** fly-out children
/// (`view.theme.<name>`). Parses each shipped default theme's `"name"` field
/// straight off [`crate::theme::DEFAULT_THEMES`] (the same JSON the
/// `ThemeManager` loads at startup), so the menu reflects the built-in set
/// without needing a live gpui context. (User-added themes are not visible to
/// this static builder; the C++ default state is exactly the built-ins.)
pub fn theme_display_names() -> Vec<String> {
    crate::theme::DEFAULT_THEMES
        .iter()
        .filter_map(|(_, json)| {
            serde_json::from_str::<serde_json::Value>(json)
                .ok()?
                .get("name")?
                .as_str()
                .map(|s| s.to_string())
        })
        .collect()
}

/// Build the **Examples ▸** fly-out children: one leaf per bundled example,
/// `file.example.<name>` (the start-page Examples bucket, surfaced in the File
/// menu). Reads [`crate::ui::examples::examples`].
fn example_menu_items() -> Vec<MenuNode> {
    crate::ui::examples::examples()
        .iter()
        .map(|(name, _)| MenuNode::item(name, "", &format!("file.example.{name}")))
        .collect()
}

/// Build the **View ▸ Theme ▸** fly-out children: one checkable-style leaf per
/// available theme, `view.theme.<name>` (the active theme is shown checked via
/// the live checked-set, not baked into the tree), followed by a separator and
/// the C++ "Edit Theme…" entry (`view.theme_edit`; main.cpp:1332-1333).
fn theme_menu_items() -> Vec<MenuNode> {
    let mut items: Vec<MenuNode> = theme_display_names()
        .into_iter()
        .map(|name| MenuNode::item(&name, "", &format!("view.theme.{name}")))
        .collect();
    items.push(MenuNode::Separator);
    items.push(MenuNode::item("Edit Theme…", "", "view.theme_edit"));
    items
}

/// The Reclass menu bar as data (app-shell §7 `createMenus`) — the full command
/// set + shortcuts the palette searches and the menu bar renders. Mirrors the
/// C++ menus (`_design`/reclass_reference crops): the File menu uses **cascading
/// submenus** (Recent Files ▸ · Import ▸ · Export ▸ · Examples ▸ · Data Source ▸),
/// the View menu carries the checkable display toggles plus Font ▸ / Theme ▸.
/// Examples ▸ children come from the bundled set; Theme ▸ children from the
/// shipped themes.
pub fn default_menu_tree() -> Vec<MenuNode> {
    menu_tree_with(&[], &[])
}

/// One Recent-Files entry: the absolute path (its file name is the menu label,
/// its full path the command suffix). Mirrors the C++ `recentFiles` QSettings
/// list rendered by `updateRecentFilesMenu` (main.cpp:8780).
#[derive(Clone, Debug)]
pub struct RecentMenuEntry {
    /// The label shown in the menu (the file name).
    pub label: String,
    /// The command id (`file.recent.<index>`); the host maps it back to a path.
    pub command: String,
}

/// One Data-Source menu entry built dynamically from the controller's saved
/// sources (the C++ `populateSourceMenu` rows; main.cpp:8802). `active` drives
/// the live checkmark.
#[derive(Clone, Debug)]
pub struct SourceMenuEntry {
    pub label: String,
    pub command: String,
    pub active: bool,
}

/// Build one Recent-Files menu row label: the 1-based index + the file name,
/// "1  foo.rcx" (the C++ `&%1  %2` numbered accelerator label;
/// `updateRecentFilesMenu`, main.cpp:8790, minus the `&` mnemonic). Pure;
/// unit-tested. `index` is the zero-based position in the recent list.
fn recent_menu_label(index: usize, file_name: &str) -> String {
    format!("{}  {}", index + 1, file_name)
}

/// The Reclass menu bar as data, with the **dynamic** Recent-Files and saved
/// Data-Source rows supplied by the host. `default_menu_tree()` passes empties
/// (the construction-time tree); the window rebuilds with live data via
/// [`MenuBar::set_menus`](crate::ui::menubar::MenuBar::set_menus).
pub fn menu_tree_with(recent: &[RecentMenuEntry], sources: &[SourceMenuEntry]) -> Vec<MenuNode> {
    use MenuNode as N;
    // Recent Files children — the C++ shows "(empty)" disabled when none, and
    // otherwise renders each row as "&N  filename" (a 1-based accelerator index +
    // the file name; `updateRecentFilesMenu`, main.cpp:8790). We mirror the
    // numbered prefix so the menu reads "1  foo.rcx", "2  bar.rcx", … (the `&`
    // mnemonic is dropped — this port shows the bare number, matching the rest of
    // the bar's de-mnemonic'd titles).
    let recent_children: Vec<MenuNode> = if recent.is_empty() {
        vec![N::disabled_item("(empty)", "", "file.recent.empty")]
    } else {
        recent
            .iter()
            .enumerate()
            .map(|(i, e)| N::item(&recent_menu_label(i, &e.label), "", &e.command))
            .collect()
    };
    // Data Source children — the File source, then the registered built-in
    // providers, then any saved sources (active one rendered checked via the
    // host's checked-set), then Clear All (the C++ `populateSourceMenu` layout;
    // main.cpp:8802 → `ProviderRegistry::populateSourceMenu`,
    // providerregistry.cpp:63). The C++ provider set is exactly File + the
    // registered providers (processmemory / remoteprocessmemory / windbgmemory /
    // reclass.netcompatlayer — see `s_providerIcons`, providerregistry.cpp:66).
    // There is NO "Kernel Memory" data-source row: `kernelmemory` is only a
    // provider-tab id reached from the right-click Browse-Page-Tables path
    // (controller.cpp:4029/4066), never emitted into the Data Source menu.
    let mut source_children = vec![
        N::item("File", "", "source.file"),
        N::item("Process Memory", "", "source.process"),
        N::item("Remote Process Memory", "", "source.remote"),
        N::item("WinDbg Memory", "", "source.windbg"),
        N::item("ReClass.NET Compat", "", "source.rcnet"),
    ];
    if !sources.is_empty() {
        source_children.push(N::Separator);
        for s in sources {
            source_children.push(N::item(&s.label, "", &s.command));
        }
    }
    source_children.push(N::Separator);
    source_children.push(N::item("Clear All", "", "source.clear"));
    vec![
        N::submenu(
            "&File",
            vec![
                N::item("New Class", "Ctrl+N", "file.new_class"),
                N::item("New Struct", "Ctrl+T", "file.new_struct"),
                N::item("New Enum", "Ctrl+E", "file.new_enum"),
                N::item("Open…", "Ctrl+O", "file.open"),
                // Dynamic — the host rebuilds the tree with the live recent list
                // (or a disabled "(empty)" row) via `MenuBar::set_menus`.
                N::submenu("Recent Files", recent_children),
                N::Separator,
                N::item("Save", "Ctrl+S", "file.save"),
                // Save As is QKeySequence::SaveAs (Ctrl+Shift+S on Win/Linux).
                N::item("Save As…", "Ctrl+Shift+S", "file.save_as"),
                N::Separator,
                N::submenu(
                    "Import",
                    vec![
                        N::item("From Source…", "", "file.import.source"),
                        N::item("ReClass XML…", "", "file.import.xml"),
                        N::item("PDB…", "", "file.import.pdb"),
                    ],
                ),
                N::submenu(
                    "Export",
                    vec![
                        N::item("C++ Header…", "", "file.export.cpp"),
                        N::item("Rust Structs…", "", "file.export.rust"),
                        N::item("#define Offsets…", "", "file.export.defines"),
                        N::item("C# Structs…", "", "file.export.csharp"),
                        N::item("Python ctypes…", "", "file.export.python"),
                        N::item("ReClass XML…", "", "file.export.xml"),
                    ],
                ),
                N::submenu("Examples", example_menu_items()),
                N::Separator,
                N::item("Close Project", "Ctrl+W", "file.close"),
                N::Separator,
                N::submenu("Data Source", source_children),
                N::Separator,
                N::item("Exit", "", "file.exit"),
            ],
        ),
        N::submenu(
            "&Edit",
            vec![
                // C++ Edit menu (main.cpp:1188-1215): Undo / Redo / —— / Add
                // Bookmark… (Ctrl+B) / Quick Bookmark Here (Ctrl+Alt+B). The Rust
                // tree previously invented a Cut/Copy/Paste/Delete/Select-All set
                // that has no C++ counterpart; replaced to match parity.
                N::item("Undo", "Ctrl+Z", "edit.undo"),
                // Redo is QKeySequence::Redo — Ctrl+Y (Windows/Linux) or
                // Ctrl+Shift+Z. Show the primary platform sequence.
                N::item("Redo", "Ctrl+Y", "edit.redo"),
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
                // C++ Font submenu (main.cpp:1300-1315) is an exclusive
                // font-FAMILY picker persisted to settings("font") and applied
                // via setEditorFont — NOT a rem-scaling control. The active
                // family is shown checked via the live checked-set.
                N::submenu(
                    "Font",
                    vec![
                        N::item("Consolas", "", "view.font.consolas"),
                        N::item("JetBrains Mono", "", "view.font.jetbrains"),
                    ],
                ),
                N::submenu("Theme", theme_menu_items()),
                N::Separator,
                N::item("Compact Columns", "", "view.compact_columns"),
                N::item("Tree Lines", "", "view.tree_lines"),
                N::item("Relative Offsets", "", "view.relative_offsets"),
                N::item("Type Hints", "", "view.type_hints"),
                N::item("Comments", "", "view.comments"),
                N::item("Hover Effects", "", "view.hover"),
                N::item("Minimap", "", "view.minimap"),
                N::Separator,
                N::item("Refresh", "F5", "view.refresh"),
                N::item("Go to Address…", "Ctrl+G", "view.goto_address"),
                N::item("Command Palette…", "Ctrl+K", "view.command_palette"),
                N::Separator,
                N::item("Split Editor", "Ctrl+\\", "view.split"),
                N::item("Unsplit Editor", "Ctrl+Shift+\\", "view.unsplit"),
                N::Separator,
                N::item("Project", "", "view.project"),
                // C++ binds Memory Scanner to Ctrl+Shift+S (main.cpp:1452), but in
                // this single global key context that collides with Save As
                // (QKeySequence::SaveAs = Ctrl+Shift+S). The port keeps the scanner
                // on Ctrl+Shift+M and reserves Ctrl+Shift+S for Save As; the label
                // matches the actual binding.
                N::item("Memory Scanner", "Ctrl+Shift+M", "view.scanner"),
                N::item("Modules", "Ctrl+Shift+Y", "view.modules"),
                N::item("Bookmarks", "Ctrl+Shift+B", "view.bookmarks"),
                N::Separator,
                // C++ Presentation Mode is Ctrl+Shift+P (main.cpp:1510) but in
                // this port Ctrl+Shift+P is the Zed Command Palette trigger; keep
                // Presentation Mode reachable without a colliding accelerator.
                N::item("Presentation Mode", "", "view.presentation"),
            ],
        ),
        // C++ Tools menu (main.cpp:1522-1572): RTTI Browser (Ctrl+Shift+R),
        // Type Aliases…, Performance Profiler… (Ctrl+Shift+F), —— , Start/Stop
        // MCP Server, —— , Options…. The previous tree invented a "Validate
        // Project… Ctrl+Shift+V" item with no C++ counterpart (removed). The MCP
        // label is dynamic (Start vs Stop); the live label is pushed by the host.
        N::submenu(
            "&Tools",
            vec![
                N::item("RTTI Browser", "Ctrl+Shift+R", "tools.rtti"),
                N::item("Type Aliases…", "", "tools.type_aliases"),
                N::item("Performance Profiler…", "Ctrl+Shift+F", "tools.profiler"),
                N::Separator,
                N::item("Start MCP Server", "", "tools.mcp"),
                N::Separator,
                N::item("Options…", "", "tools.options"),
            ],
        ),
        N::submenu(
            "&Plugins",
            vec![N::item("Manage Plugins…", "", "plugins.manage")],
        ),
        // C++ Help menu (main.cpp:1579-1584): Keyboard Shortcuts… (F1), —— ,
        // About Reclass. The previous tree invented a "Documentation" item that
        // has no C++ counterpart (replaced with the real Shortcuts item).
        N::submenu(
            "&Help",
            vec![
                N::item("Keyboard Shortcuts…", "F1", "help.shortcuts"),
                N::Separator,
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
                _subscription: subscription,
            }
        }

        /// Read-only access to the model (tests / wiring).
        pub fn model(&self) -> &PaletteModel {
            &self.model
        }

        /// Focus the query input so the very first keystroke types into the
        /// palette (the blocker fix: without this the host focuses the view's
        /// handle, keystrokes fall through to the window's global shortcuts, and
        /// fuzzy filtering never runs). The host calls this right after opening
        /// the modal; we also expose the same handle through [`Focusable`] so
        /// `window.focus(&palette.focus_handle(cx))` lands on the input too.
        pub fn focus_input(&self, window: &mut Window, cx: &mut App) {
            self.input.read(cx).focus_handle(cx).focus(window, cx);
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

        /// Intercept the navigation/confirm/cancel keys in the **capture** phase
        /// (before the focused query `Input` consumes them for cursor movement).
        /// The input owns focus so typing filters, but Up/Down move the selected
        /// row, Enter triggers, Esc dismisses — exactly the C++ `eventFilter`
        /// that forwarded these from the line-edit to the list (§8). Returns
        /// `true` when handled so the caller stops propagation.
        fn handle_nav_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
            match key {
                "down" | "pagedown" => {
                    self.model.move_down();
                    cx.notify();
                    true
                }
                "up" | "pageup" => {
                    self.model.move_up();
                    cx.notify();
                    true
                }
                "enter" => {
                    if let Some(cmd) = self.model.activate_current() {
                        cx.emit(PaletteEvent::Trigger(cmd));
                    }
                    true
                }
                "escape" => {
                    cx.emit(PaletteEvent::Cancel);
                    true
                }
                _ => false,
            }
        }
    }

    impl Focusable for CommandPalette {
        /// Delegate to the query input's focus handle so the host's
        /// `window.focus(&palette.focus_handle(cx))` focuses the *input* (not an
        /// inert wrapper handle) — typed characters then reach the palette
        /// instead of falling through to the window's global shortcuts.
        fn focus_handle(&self, cx: &App) -> FocusHandle {
            self.input.read(cx).focus_handle(cx)
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
                .key_context("RcxCommandPalette")
                // Capture-phase key handling: the query input owns focus (so
                // typing filters), so Up/Down/Enter/Esc reach it first and would
                // otherwise move the text cursor. Intercept them here, before the
                // input, to drive list navigation / confirm / cancel.
                .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
                    if this.handle_nav_key(ev.keystroke.key.as_str(), cx) {
                        cx.stop_propagation();
                    }
                }))
                .on_action(cx.listener(Self::on_down))
                .on_action(cx.listener(Self::on_up))
                .on_action(cx.listener(Self::on_confirm))
                .on_action(cx.listener(Self::on_cancel))
                .w_full()
                .max_w(px(600.))
                .max_h(px(460.))
                .bg(color::elevated_bg(cx))
                .rounded(px(tokens::radius::XL))
                .border_1()
                .border_color(border)
                .shadow_lg()
                .overflow_hidden()
                .child(
                    gpui_component::h_flex()
                        .px(px(tokens::space::LG))
                        .py(px(tokens::space::MD))
                        .gap(px(tokens::space::MD))
                        .items_center()
                        .border_b_1()
                        .border_color(border)
                        .child(
                            div()
                                .flex_none()
                                .text_color(color::text_muted(cx))
                                .child(crate::ui::design::icon::search().size_4()),
                        )
                        .child(div().flex_1().child(Input::new(&self.input).w_full())),
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
        assert!(cmds.contains(&"edit.add_bookmark"));
        assert!(cmds.contains(&"edit.quick_bookmark"));
        assert!(cmds.contains(&"view.goto_address"));
        assert!(cmds.contains(&"view.command_palette"));
        assert!(cmds.contains(&"tools.options"));
        // With no recents, the Recent Files submenu contributes only the disabled
        // "(empty)" placeholder (never a live, activatable recent entry).
        let recent: Vec<&&str> = cmds.iter().filter(|c| c.contains("recent")).collect();
        assert_eq!(recent, vec![&"file.recent.empty"]);
    }

    #[test]
    fn file_menu_uses_cascading_submenus() {
        // The File menu's Import/Export/Examples/Data Source are real submenus
        // (cascading fly-outs), so their leaves flatten under those paths.
        let entries = flatten_menu_bar(&default_menu_tree());
        let path_of = |cmd: &str| {
            entries
                .iter()
                .find(|e| e.command == cmd)
                .map(|e| e.path.clone())
        };
        assert_eq!(
            path_of("file.import.source").as_deref(),
            Some("File > Import > From Source…")
        );
        assert_eq!(
            path_of("file.export.cpp").as_deref(),
            Some("File > Export > C++ Header…")
        );
        assert_eq!(
            path_of("source.file").as_deref(),
            Some("File > Data Source > File")
        );
    }

    #[test]
    fn examples_submenu_has_one_item_per_bundled_example() {
        let entries = flatten_menu_bar(&default_menu_tree());
        for (name, _) in crate::ui::examples::examples() {
            let cmd = format!("file.example.{name}");
            assert!(
                entries.iter().any(|e| e.command == cmd),
                "missing example command {cmd}"
            );
        }
    }

    #[test]
    fn view_menu_has_checkable_toggles_and_theme_children() {
        let entries = flatten_menu_bar(&default_menu_tree());
        let cmds: Vec<&str> = entries.iter().map(|e| e.command.as_str()).collect();
        for c in [
            "view.compact_columns",
            "view.tree_lines",
            "view.relative_offsets",
            "view.type_hints",
            "view.comments",
            "view.hover",
            "view.minimap",
            "view.font.consolas",
            "view.font.jetbrains",
            "view.split",
            "view.presentation",
            "view.theme_edit",
        ] {
            assert!(cmds.contains(&c), "View menu missing {c}");
        }
        // One Theme child per shipped theme.
        for name in super::theme_display_names() {
            let cmd = format!("view.theme.{name}");
            assert!(
                entries.iter().any(|e| e.command == cmd),
                "missing theme command {cmd}"
            );
        }
    }

    #[test]
    fn edit_menu_matches_cpp_bookmark_structure() {
        // The Edit menu is Undo/Redo/——/Add Bookmark…/Quick Bookmark Here — the
        // C++ structure (main.cpp:1188). The invented clipboard items are gone.
        let entries = flatten_menu_bar(&default_menu_tree());
        let cmds: Vec<&str> = entries.iter().map(|e| e.command.as_str()).collect();
        assert!(cmds.contains(&"edit.undo"));
        assert!(cmds.contains(&"edit.redo"));
        assert!(cmds.contains(&"edit.add_bookmark"));
        assert!(cmds.contains(&"edit.quick_bookmark"));
        // No phantom clipboard/select-all/validate/docs entries.
        for gone in [
            "edit.cut",
            "edit.copy",
            "edit.paste",
            "edit.select_all",
            "tools.validate",
            "help.docs",
        ] {
            assert!(!cmds.contains(&gone), "{gone} should be removed");
        }
    }

    #[test]
    fn tools_menu_advertises_rtti_and_profiler_shortcuts() {
        // The C++ Tools menu binds QKeySequence(Ctrl|Shift|R) for the RTTI Browser
        // (main.cpp:1524) and QKeySequence(Ctrl|Shift|F) for the Performance
        // Profiler (main.cpp:1565). The menu label advertises those accelerators;
        // the window registers the matching global key bindings (so the shortcuts
        // are no longer dead). This pins the advertised label ⇄ command contract
        // the bindings route through.
        let entries = flatten_menu_bar(&default_menu_tree());
        let rtti = entries
            .iter()
            .find(|e| e.command == "tools.rtti")
            .expect("RTTI Browser entry present");
        assert_eq!(rtti.shortcut, "Ctrl+Shift+R");
        let profiler = entries
            .iter()
            .find(|e| e.command == "tools.profiler")
            .expect("Performance Profiler entry present");
        assert_eq!(profiler.shortcut, "Ctrl+Shift+F");
    }

    #[test]
    fn data_source_menu_matches_cpp_provider_set() {
        // The C++ `ProviderRegistry::populateSourceMenu` (providerregistry.cpp:63)
        // emits File + the registered providers + saved sources + Clear All — the
        // registered provider set being exactly processmemory / remoteprocessmemory
        // / windbgmemory / reclass.netcompatlayer (`s_providerIcons`). There is NO
        // "Kernel Memory" data-source row: `kernelmemory` is only a provider-tab id
        // used by the right-click Browse-Page-Tables path, never a Data-Source entry.
        let entries = flatten_menu_bar(&default_menu_tree());
        let cmds: Vec<&str> = entries.iter().map(|e| e.command.as_str()).collect();
        // File + the four registered providers are present.
        for c in [
            "source.file",
            "source.process",
            "source.remote",
            "source.windbg",
            "source.rcnet",
            "source.clear",
        ] {
            assert!(cmds.contains(&c), "Data Source menu missing {c}");
        }
        // The invented Kernel Memory row (no C++ counterpart) is gone.
        assert!(
            !cmds.contains(&"source.kernel"),
            "source.kernel should be removed — kernelmemory is never a Data Source row"
        );
        assert!(
            !entries.iter().any(|e| e.path.ends_with("Kernel Memory")),
            "no 'Kernel Memory' Data Source label should remain"
        );
    }

    #[test]
    fn dynamic_recent_and_source_rows_appear() {
        use super::{menu_tree_with, RecentMenuEntry, SourceMenuEntry};
        let recent = vec![RecentMenuEntry {
            label: "foo.rcx".into(),
            command: "file.recent.0".into(),
        }];
        let sources = vec![SourceMenuEntry {
            label: "File 'game.bin'".into(),
            command: "source.saved.0".into(),
            active: true,
        }];
        let entries = flatten_menu_bar(&menu_tree_with(&recent, &sources));
        let cmds: Vec<&str> = entries.iter().map(|e| e.command.as_str()).collect();
        assert!(cmds.contains(&"file.recent.0"), "recent row missing");
        assert!(cmds.contains(&"source.saved.0"), "saved-source row missing");
        // Empty recent list → a disabled "(empty)" placeholder, never a live row.
        let empty = flatten_menu_bar(&menu_tree_with(&[], &[]));
        let placeholder = empty
            .iter()
            .find(|e| e.command == "file.recent.empty")
            .expect("empty placeholder present");
        assert!(!placeholder.enabled);
    }

    #[test]
    fn recent_menu_label_numbers_rows_one_based() {
        use super::recent_menu_label;
        assert_eq!(recent_menu_label(0, "foo.rcx"), "1  foo.rcx");
        assert_eq!(recent_menu_label(1, "bar.rcx"), "2  bar.rcx");
        assert_eq!(recent_menu_label(9, "tenth.rcx"), "10  tenth.rcx");
    }

    #[test]
    fn recent_submenu_rows_render_numbered_labels() {
        use super::{menu_tree_with, RecentMenuEntry};
        let recent = vec![
            RecentMenuEntry {
                label: "alpha.rcx".into(),
                command: "file.recent.0".into(),
            },
            RecentMenuEntry {
                label: "beta.rcx".into(),
                command: "file.recent.1".into(),
            },
        ];
        let entries = flatten_menu_bar(&menu_tree_with(&recent, &[]));
        // The flattened path carries the numbered display label (the C++
        // "&N  filename" rendered as "N  filename").
        assert!(
            entries
                .iter()
                .any(|e| e.path == "File > Recent Files > 1  alpha.rcx"),
            "first recent row should be numbered '1  alpha.rcx'"
        );
        assert!(
            entries
                .iter()
                .any(|e| e.path == "File > Recent Files > 2  beta.rcx"),
            "second recent row should be numbered '2  beta.rcx'"
        );
    }

    #[test]
    fn theme_display_names_are_nonempty_and_include_default() {
        let names = super::theme_display_names();
        assert!(!names.is_empty(), "expected bundled theme names");
        assert!(
            names.iter().any(|n| n == "Zed One Dark"),
            "default theme name should be present"
        );
    }
}
