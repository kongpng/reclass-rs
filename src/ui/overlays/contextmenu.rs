//! Context menus — the node / hex-byte / workspace right-click menus
//! (`editor.cpp` `customContextMenuRequested`, `main.cpp` workspace-tree menu,
//! widgets-dialogs.md §0 "context menus").
//!
//! Port of the right-click menu *contents* + their selection-dependent logic.
//! Per the cookbook (ARCHITECTURE §5) the rendering maps onto
//! `ContextMenuExt` + `PopupMenu`; the gpui side needs an `Action` per item, so
//! this module models the menus as **data** ([`MenuItem`] catalogues, keyed by a
//! [`CommandId`](crate::ui::pickers::commandpalette::CommandId)) + the load-bearing logic
//! (single-vs-multi selection, class↔struct convert, pin/unpin label, delete
//! label pluralization) — all unit-tested — and the host turns the item list into
//! a `PopupMenu`.
//!
//! Gated behind the `ui` feature only for the (small) menu-builder helper; the
//! catalogues + logic are always built/tested.

use crate::ui::pickers::commandpalette::CommandId;

/// One entry in a context menu — a labeled command, a checkable toggle, or a
/// separator. Mirrors a `menu.addAction(...)` / `addSeparator()`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MenuItem {
    /// A command item: visible label + the [`CommandId`] to route + enabled.
    Action {
        label: String,
        command: CommandId,
        enabled: bool,
    },
    /// A checkable item (e.g. "Relative Offsets"): label + command + checked.
    Check {
        label: String,
        command: CommandId,
        checked: bool,
    },
    /// A separator (`addSeparator`).
    Separator,
}

impl MenuItem {
    /// An enabled command item.
    pub fn action(label: &str, command: &str) -> MenuItem {
        MenuItem::Action {
            label: label.to_string(),
            command: command.to_string(),
            enabled: true,
        }
    }

    /// A checkable item.
    pub fn check(label: &str, command: &str, checked: bool) -> MenuItem {
        MenuItem::Check {
            label: label.to_string(),
            command: command.to_string(),
            checked,
        }
    }
}

/// The hex-byte selection context menu (`editor.cpp:1919-1934`): the actions over
/// a hex-byte selection. The C++ enables paste/edit/zero/clear vs copy-only based
/// on whether there is data; here `has_selection` toggles the selection-only ones.
pub fn hex_selection_menu(has_selection: bool) -> Vec<MenuItem> {
    vec![
        MenuItem::action("Break into new class", "hex.break_into_class"),
        MenuItem::Separator,
        MenuItem::action("Copy as hex", "hex.copy_hex"),
        MenuItem::action("Copy as C array", "hex.copy_c_array"),
        MenuItem::action("Copy as Python bytes", "hex.copy_python"),
        MenuItem::action("Save as binary file…", "hex.save_binary"),
        MenuItem::Separator,
        MenuItem::action("Paste hex", "hex.paste"),
        MenuItem::action("Edit hex…", "hex.edit"),
        MenuItem::action("Zero-fill", "hex.zero_fill"),
        MenuItem::Separator,
        {
            // "Clear selection" only meaningful with an active selection.
            MenuItem::Action {
                label: "Clear selection".to_string(),
                command: "hex.clear_selection".to_string(),
                enabled: has_selection,
            }
        },
    ]
}

/// The offset-mode context menu (`editor.cpp:1961-1963`): toggle relative vs
/// absolute offsets (the gutter right-click).
pub fn offset_mode_menu(relative: bool) -> Vec<MenuItem> {
    vec![
        MenuItem::check("Relative Offsets (+0x)", "view.relative_offsets", relative),
        MenuItem::check("Absolute Addresses", "view.absolute_offsets", !relative),
    ]
}

/// The command-row keyword-convert menu (`editor.cpp:1991-1995`): class↔struct
/// convert (enum has no conversion). Returns the single available convert item, or
/// empty for an enum / unknown keyword.
pub fn keyword_convert_menu(keyword: &str) -> Vec<MenuItem> {
    match keyword {
        "class" => vec![MenuItem::action("Convert to Struct", "type.convert_struct")],
        "struct" => vec![MenuItem::action("Convert to Class", "type.convert_class")],
        _ => Vec::new(), // enum: no conversion
    }
}

/// Whether a struct keyword can be converted (class↔struct only).
pub fn can_convert_keyword(keyword: &str) -> bool {
    keyword == "class" || keyword == "struct"
}

/// The workspace-tree node context menu (`main.cpp:7221-7267`): navigation +
/// convert + find-refs are **single-selection only**; pin/unpin + delete work for
/// any selection. `count` is the number of selected top-level types; `keyword` is
/// the single selection's class keyword (ignored for multi); `all_pinned` drives
/// the Pin/Unpin label.
pub fn workspace_node_menu(count: usize, keyword: &str, all_pinned: bool) -> Vec<MenuItem> {
    let mut items = Vec::new();
    if count == 0 {
        return items;
    }

    // Navigation + duplicate (single only).
    if count == 1 {
        items.push(MenuItem::action("Open in Current Tab", "node.open_current"));
        items.push(MenuItem::action("Open in New Tab", "node.open_new"));
        items.push(MenuItem::action("Duplicate", "node.duplicate"));
        items.push(MenuItem::Separator);

        // Convert (single, class↔struct only).
        match keyword {
            "class" => items.push(MenuItem::action("Convert to Struct", "node.convert_struct")),
            "struct" => items.push(MenuItem::action("Convert to Class", "node.convert_class")),
            _ => {}
        }

        // Find References (single only).
        items.push(MenuItem::action("Find References", "node.find_references"));
    }

    // Pin/Unpin (any selection).
    items.push(MenuItem::action(
        if all_pinned { "Unpin" } else { "Pin" },
        "node.toggle_pin",
    ));

    items.push(MenuItem::Separator);

    // Delete (any selection; label pluralizes).
    let del_label = if count == 1 {
        "Delete".to_string()
    } else {
        format!("Delete {count} items")
    };
    items.push(MenuItem::Action {
        label: del_label,
        command: "node.delete".to_string(),
        enabled: true,
    });

    items
}

/// The process-picker row context menu (`processpicker` right-click): copy the
/// row's PID / Name / Path to the clipboard. The "Copy PID" item is disabled for
/// synthetic (non-process) rows that have no PID (`has_pid == false`). This is the
/// menu *data*; the process-picker view turns it into a `PopupMenu` and copies the
/// corresponding field on activation.
pub fn process_row_menu(has_pid: bool) -> Vec<MenuItem> {
    vec![
        MenuItem::Action {
            label: "Copy PID".to_string(),
            command: "process.copy_pid".to_string(),
            enabled: has_pid,
        },
        MenuItem::action("Copy Name", "process.copy_name"),
        MenuItem::action("Copy Path", "process.copy_path"),
    ]
}

// ── gpui menu builder (feature-gated) ────────────────────────────────────────

/// Build a gpui-component `PopupMenu` from a [`MenuItem`] list, routing each item
/// to `on_command(CommandId)`. The action plumbing (each item needs a gpui
/// `Action`) is handled by a single generic [`MenuCommand`] action carrying the
/// command id, dispatched on click.
///
/// This is the seam the editor/workspace right-click handlers call via
/// `element.context_menu(|menu, window, cx| build_popup_menu(menu, &items, ...))`.
#[cfg(feature = "ui")]
pub fn menu_item_labels(items: &[MenuItem]) -> Vec<Option<(String, CommandId, bool)>> {
    // A flattened view the view layer maps to PopupMenu rows: `None` = separator,
    // `Some((label, command, enabled))` = an actionable row. (Checkables collapse
    // to actionable rows carrying their command; the toggle state is reflected in
    // the label by the caller.)
    items
        .iter()
        .map(|it| match it {
            MenuItem::Separator => None,
            MenuItem::Action {
                label,
                command,
                enabled,
            } => Some((label.clone(), command.clone(), *enabled)),
            MenuItem::Check {
                label,
                command,
                checked,
            } => {
                let mark = if *checked { "\u{2713} " } else { "  " };
                Some((format!("{mark}{label}"), command.clone(), true))
            }
        })
        .collect()
}

/// A short keystroke hint shown right-aligned on a menu row (e.g. "Ctrl+B").
/// Used by the view to add Zed-style keybind hints; the pure menu builders don't
/// carry shortcuts (the C++ context menus mostly don't show them), so this is a
/// view-side lookup keyed by [`CommandId`].
#[cfg(feature = "ui")]
pub fn context_shortcut_for(command: &str) -> &'static str {
    match command {
        "hex.break_into_class" => "Ctrl+R",
        "hex.copy_hex" => "Ctrl+C",
        "hex.paste" => "Ctrl+V",
        "node.duplicate" => "Ctrl+D",
        "node.delete" => "Del",
        "node.find_references" => "Shift+F12",
        _ => "",
    }
}

/// Commands that are destructive (rendered in the danger color, Zed-style).
#[cfg(feature = "ui")]
pub fn is_danger_command(command: &str) -> bool {
    matches!(
        command,
        "node.delete" | "hex.zero_fill" | "hex.clear_selection"
    )
}

// ── gpui context-menu view (feature-gated) ───────────────────────────────────

#[cfg(feature = "ui")]
pub use view::{ContextMenuEvent, ContextMenuView};

#[cfg(feature = "ui")]
mod view {
    use super::{context_shortcut_for, is_danger_command, CommandId, MenuItem};
    use crate::ui::design::{color, tokens};
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::{Icon, IconName};

    /// The leading SVG icon for a context-menu command (the C++ menus paint an
    /// icon next to each action). Maps each [`CommandId`] family to a verified
    /// [`IconName`]; commands without a natural icon return `None` (the row keeps
    /// an empty leading slot so labels stay aligned).
    fn command_icon(command: &str) -> Option<IconName> {
        let name = match command {
            "hex.break_into_class" => IconName::Frame,
            "hex.copy_hex" | "hex.copy_c_array" | "hex.copy_python" => IconName::Copy,
            "hex.save_binary" => IconName::HardDrive,
            "hex.paste" => IconName::Inbox,
            "hex.edit" => IconName::Replace,
            "hex.zero_fill" => IconName::Minus,
            "hex.clear_selection" => IconName::Close,
            "node.open_current" => IconName::ArrowRight,
            "node.open_new" => IconName::ExternalLink,
            "node.duplicate" => IconName::Copy,
            "node.convert_struct"
            | "node.convert_class"
            | "type.convert_struct"
            | "type.convert_class" => IconName::Replace,
            "node.find_references" => IconName::Search,
            "node.toggle_pin" => IconName::Star,
            "node.delete" => IconName::Delete,
            _ => return None,
        };
        Some(name)
    }

    /// The menu's outcome.
    #[derive(Clone, Debug)]
    pub enum ContextMenuEvent {
        /// An item was chosen — route its [`CommandId`].
        Activated(CommandId),
        /// Dismissed (clicked outside / Esc).
        Dismissed,
    }

    /// A free-standing Zed-styled right-click menu rendered from a [`MenuItem`]
    /// list. The host anchors this (via the overlay/popover layer) at the click
    /// point; it emits [`ContextMenuEvent`] for the chosen command. (The C++ uses
    /// `QMenu` / `ContextMenuExt`; this is the gpui equivalent with the shared
    /// elevated-surface look: 6px radius, 1px border, soft shadow, inset rows,
    /// hover overlay, separators, a left check/icon slot, and a right keybind slot.)
    pub struct ContextMenuView {
        items: Vec<MenuItem>,
        focus_handle: FocusHandle,
        /// The keyboard-highlighted item index (item 11): Up/Down move it over
        /// selectable rows, Enter activates it. `None` until the first key/hover.
        selected: Option<usize>,
    }

    impl ContextMenuView {
        /// Build a context menu over the given items.
        pub fn new(items: Vec<MenuItem>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
            ContextMenuView {
                items,
                focus_handle: cx.focus_handle(),
                selected: None,
            }
        }

        /// Read-only access to the items (tests / wiring).
        pub fn items(&self) -> &[MenuItem] {
            &self.items
        }

        /// The currently keyboard-highlighted item index (tests / wiring).
        pub fn selected(&self) -> Option<usize> {
            self.selected
        }

        /// Whether an item index is activatable (not a separator).
        fn is_selectable(&self, i: usize) -> bool {
            matches!(
                self.items.get(i),
                Some(MenuItem::Action { enabled: true, .. }) | Some(MenuItem::Check { .. })
            )
        }

        /// The next selectable item from `from` in direction `dir` (skips
        /// separators + disabled actions). `None` if none in that direction.
        fn next_selectable(&self, from: i32, dir: i32) -> Option<usize> {
            let len = self.items.len() as i32;
            let mut i = from + dir;
            while i >= 0 && i < len {
                if self.is_selectable(i as usize) {
                    return Some(i as usize);
                }
                i += dir;
            }
            None
        }

        /// The first selectable item (top-down).
        fn first_selectable(&self) -> Option<usize> {
            (0..self.items.len()).find(|&i| self.is_selectable(i))
        }

        /// The last selectable item.
        fn last_selectable(&self) -> Option<usize> {
            (0..self.items.len()).rev().find(|&i| self.is_selectable(i))
        }

        fn activate(&mut self, command: CommandId, cx: &mut Context<Self>) {
            cx.emit(ContextMenuEvent::Activated(command));
        }

        /// Activate the keyboard-highlighted item, if any.
        fn activate_selected(&mut self, cx: &mut Context<Self>) {
            let Some(i) = self.selected else { return };
            match self.items.get(i) {
                Some(MenuItem::Action {
                    command,
                    enabled: true,
                    ..
                })
                | Some(MenuItem::Check { command, .. }) => {
                    let cmd = command.clone();
                    self.activate(cmd, cx);
                }
                _ => {}
            }
        }

        /// Keyboard handler (item 11): Up/Down highlight selectable items, Enter
        /// activates the highlighted one, Esc dismisses. Returns `true` when
        /// handled.
        fn handle_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
            match key {
                "down" => {
                    let from = self.selected.map(|s| s as i32).unwrap_or(-1);
                    if let Some(next) = self.next_selectable(from, 1) {
                        self.selected = Some(next);
                    } else if self.selected.is_none() {
                        self.selected = self.first_selectable();
                    }
                    cx.notify();
                    true
                }
                "up" => {
                    let from = self
                        .selected
                        .map(|s| s as i32)
                        .unwrap_or(self.items.len() as i32);
                    if let Some(prev) = self.next_selectable(from, -1) {
                        self.selected = Some(prev);
                    } else if self.selected.is_none() {
                        self.selected = self.last_selectable();
                    }
                    cx.notify();
                    true
                }
                "home" => {
                    self.selected = self.first_selectable();
                    cx.notify();
                    true
                }
                "end" => {
                    self.selected = self.last_selectable();
                    cx.notify();
                    true
                }
                "enter" | "space" => {
                    self.activate_selected(cx);
                    true
                }
                "escape" => {
                    cx.emit(ContextMenuEvent::Dismissed);
                    true
                }
                _ => false,
            }
        }

        /// Hover over an item highlights it (keeps keyboard + mouse in sync).
        fn hover_item(&mut self, i: usize, cx: &mut Context<Self>) {
            if self.selected != Some(i) && self.is_selectable(i) {
                self.selected = Some(i);
                cx.notify();
            }
        }
    }

    impl Focusable for ContextMenuView {
        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.focus_handle.clone()
        }
    }

    impl EventEmitter<ContextMenuEvent> for ContextMenuView {}

    impl Render for ContextMenuView {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let fg = color::text(cx);
            let muted = color::text_muted(cx);
            let disabled = color::text_disabled(cx);
            let danger = color::danger_emphasis(cx);
            let hover_bg = color::hover_overlay(cx);
            let sel_bg = color::selected_bg(cx);
            let selected = self.selected;

            let rows: Vec<AnyElement> = self
                .items
                .iter()
                .enumerate()
                .map(|(i, item)| match item {
                    MenuItem::Separator => div()
                        .h(px(tokens::border::THIN))
                        .my(px(tokens::space::XS))
                        .mx(px(tokens::space::SM))
                        .bg(color::border(cx))
                        .into_any_element(),
                    MenuItem::Action {
                        label,
                        command,
                        enabled,
                    } => {
                        let is_danger = is_danger_command(command);
                        let row_fg = if !enabled {
                            disabled
                        } else if is_danger {
                            danger
                        } else {
                            fg
                        };
                        let shortcut = context_shortcut_for(command);
                        let cmd = command.clone();
                        let enabled = *enabled;
                        let is_sel = selected == Some(i);
                        menu_row(i)
                            .text_color(row_fg)
                            .when(is_sel, |r| r.bg(sel_bg))
                            .when(enabled, |r| {
                                let cmd = cmd.clone();
                                r.cursor_pointer()
                                    .when(!is_sel, |r| r.hover(|s| s.bg(hover_bg)))
                                    .on_mouse_move(
                                        cx.listener(move |this, _e, _w, cx| this.hover_item(i, cx)),
                                    )
                                    .on_click(cx.listener(move |this, _e, _w, cx| {
                                        this.activate(cmd.clone(), cx)
                                    }))
                            })
                            // Leading SVG icon slot (a per-command icon, tinted to
                            // the row color; empty keeps labels aligned).
                            .child(icon_slot(command_icon(&cmd), row_fg))
                            .child(div().flex_1().min_w_0().child(label.clone()))
                            .child(shortcut_slot(shortcut, muted))
                            .into_any_element()
                    }
                    MenuItem::Check {
                        label,
                        command,
                        checked,
                    } => {
                        let cmd = command.clone();
                        let checked = *checked;
                        let is_sel = selected == Some(i);
                        menu_row(i)
                            .text_color(fg)
                            .cursor_pointer()
                            .when(is_sel, |r| r.bg(sel_bg))
                            .when(!is_sel, |r| r.hover(|s| s.bg(hover_bg)))
                            .on_mouse_move(
                                cx.listener(move |this, _e, _w, cx| this.hover_item(i, cx)),
                            )
                            .on_click(
                                cx.listener(move |this, _e, _w, cx| this.activate(cmd.clone(), cx)),
                            )
                            .child(check_slot(checked, color::accent(cx)))
                            .child(div().flex_1().min_w_0().child(label.clone()))
                            .child(shortcut_slot("", muted))
                            .into_any_element()
                    }
                })
                .collect();

            gpui_component::v_flex()
                .id("rcx-context-menu")
                .track_focus(&self.focus_handle)
                .key_context("RcxContextMenu")
                // Keyboard support (item 11): Up/Down highlight, Enter activate,
                // Esc dismiss.
                .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
                    if this.handle_key(ev.keystroke.key.as_str(), cx) {
                        cx.stop_propagation();
                    }
                }))
                .min_w(px(200.))
                .p(px(tokens::space::XS))
                .bg(color::elevated_bg(cx))
                .border_1()
                .border_color(color::border(cx))
                .rounded(px(tokens::radius::LG))
                .shadow_md()
                .text_size(px(tokens::font::UI_MD))
                .children(rows)
        }
    }

    /// A menu-item row shell (Zed inset row: 24px, comfortable padding, MD radius).
    fn menu_row(i: usize) -> Stateful<Div> {
        gpui_component::h_flex()
            .id(("ctx-row", i))
            .w_full()
            .h(px(24.))
            .px(px(tokens::space::SM))
            .gap(px(tokens::space::MD))
            .items_center()
            .rounded(px(tokens::radius::MD))
    }

    /// The left check slot — a fixed-width column holding a ✓ SVG (for a checked
    /// toggle) or nothing, so labels stay aligned.
    fn check_slot(checked: bool, color: Hsla) -> Div {
        div()
            .w(px(14.))
            .flex_none()
            .flex()
            .items_center()
            .text_color(color)
            .when(checked, |d| d.child(Icon::new(IconName::Check).size_3()))
    }

    /// The left icon slot — a fixed-width column holding a leading command SVG
    /// (tinted to `color`) or nothing, so labels stay aligned with checkables.
    fn icon_slot(icon: Option<IconName>, color: Hsla) -> Div {
        div()
            .w(px(14.))
            .flex_none()
            .flex()
            .items_center()
            .text_color(color)
            .when_some(icon, |d, name| d.child(Icon::new(name).size_3()))
    }

    /// The right keybind slot — a muted, fixed end column for the shortcut hint.
    fn shortcut_slot(shortcut: &str, color: Hsla) -> Div {
        div()
            .flex_none()
            .text_size(px(tokens::font::UI_XS))
            .text_color(color)
            .when(!shortcut.is_empty(), |d| d.child(shortcut.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        can_convert_keyword, hex_selection_menu, keyword_convert_menu, offset_mode_menu,
        workspace_node_menu, MenuItem,
    };

    fn commands(items: &[MenuItem]) -> Vec<&str> {
        items
            .iter()
            .filter_map(|it| match it {
                MenuItem::Action { command, .. } | MenuItem::Check { command, .. } => {
                    Some(command.as_str())
                }
                MenuItem::Separator => None,
            })
            .collect()
    }

    #[test]
    fn hex_menu_has_copy_and_paste_actions() {
        let items = hex_selection_menu(true);
        let cmds = commands(&items);
        assert!(cmds.contains(&"hex.copy_hex"));
        assert!(cmds.contains(&"hex.copy_c_array"));
        assert!(cmds.contains(&"hex.copy_python"));
        assert!(cmds.contains(&"hex.paste"));
        assert!(cmds.contains(&"hex.edit"));
        assert!(cmds.contains(&"hex.zero_fill"));
    }

    #[test]
    fn hex_clear_selection_disabled_without_selection() {
        let with = hex_selection_menu(true);
        let without = hex_selection_menu(false);
        let find = |items: &[MenuItem]| -> bool {
            items.iter().any(|it| {
                matches!(it, MenuItem::Action { command, enabled, .. }
                    if command == "hex.clear_selection" && *enabled)
            })
        };
        assert!(find(&with));
        assert!(!find(&without));
    }

    #[test]
    fn offset_mode_reflects_current() {
        let rel = offset_mode_menu(true);
        // Relative checked, absolute unchecked.
        assert!(matches!(&rel[0], MenuItem::Check { checked: true, .. }));
        assert!(matches!(&rel[1], MenuItem::Check { checked: false, .. }));
        let abs = offset_mode_menu(false);
        assert!(matches!(&abs[0], MenuItem::Check { checked: false, .. }));
        assert!(matches!(&abs[1], MenuItem::Check { checked: true, .. }));
    }

    #[test]
    fn keyword_convert_is_class_struct_only() {
        assert_eq!(keyword_convert_menu("class").len(), 1);
        assert_eq!(keyword_convert_menu("struct").len(), 1);
        // enum / unknown → no conversion.
        assert!(keyword_convert_menu("enum").is_empty());
        assert!(keyword_convert_menu("").is_empty());

        assert!(can_convert_keyword("class"));
        assert!(can_convert_keyword("struct"));
        assert!(!can_convert_keyword("enum"));
    }

    #[test]
    fn keyword_convert_flips_keyword() {
        match &keyword_convert_menu("class")[0] {
            MenuItem::Action { label, command, .. } => {
                assert_eq!(label, "Convert to Struct");
                assert_eq!(command, "type.convert_struct");
            }
            _ => panic!("expected action"),
        }
        match &keyword_convert_menu("struct")[0] {
            MenuItem::Action { label, .. } => assert_eq!(label, "Convert to Class"),
            _ => panic!("expected action"),
        }
    }

    #[test]
    fn workspace_menu_single_selection_full() {
        let items = workspace_node_menu(1, "struct", false);
        let cmds = commands(&items);
        // Single selection → navigation + duplicate + convert + find-refs + pin +
        // delete.
        assert!(cmds.contains(&"node.open_current"));
        assert!(cmds.contains(&"node.open_new"));
        assert!(cmds.contains(&"node.duplicate"));
        assert!(cmds.contains(&"node.convert_class")); // struct → Convert to Class
        assert!(cmds.contains(&"node.find_references"));
        assert!(cmds.contains(&"node.toggle_pin"));
        assert!(cmds.contains(&"node.delete"));
    }

    #[test]
    fn workspace_menu_multi_selection_trimmed() {
        let items = workspace_node_menu(3, "struct", false);
        let cmds = commands(&items);
        // Multi selection → only pin + delete (no navigation/convert/find-refs).
        assert!(!cmds.contains(&"node.open_current"));
        assert!(!cmds.contains(&"node.duplicate"));
        assert!(!cmds.contains(&"node.convert_class"));
        assert!(!cmds.contains(&"node.find_references"));
        assert!(cmds.contains(&"node.toggle_pin"));
        assert!(cmds.contains(&"node.delete"));
    }

    #[test]
    fn workspace_delete_label_pluralizes() {
        let single = workspace_node_menu(1, "struct", false);
        let multi = workspace_node_menu(4, "struct", false);
        let del_label = |items: &[MenuItem]| -> String {
            items
                .iter()
                .find_map(|it| match it {
                    MenuItem::Action { label, command, .. } if command == "node.delete" => {
                        Some(label.clone())
                    }
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(del_label(&single), "Delete");
        assert_eq!(del_label(&multi), "Delete 4 items");
    }

    #[test]
    fn workspace_pin_label_reflects_state() {
        let unpinned = workspace_node_menu(1, "struct", false);
        let pinned = workspace_node_menu(1, "struct", true);
        let pin_label = |items: &[MenuItem]| -> String {
            items
                .iter()
                .find_map(|it| match it {
                    MenuItem::Action { label, command, .. } if command == "node.toggle_pin" => {
                        Some(label.clone())
                    }
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(pin_label(&unpinned), "Pin");
        assert_eq!(pin_label(&pinned), "Unpin");
    }

    #[test]
    fn empty_selection_yields_empty_menu() {
        assert!(workspace_node_menu(0, "", false).is_empty());
    }

    #[test]
    fn enum_node_has_no_convert() {
        let items = workspace_node_menu(1, "enum", false);
        let cmds = commands(&items);
        assert!(!cmds.contains(&"node.convert_struct"));
        assert!(!cmds.contains(&"node.convert_class"));
        // But still has navigation + find-refs + pin + delete.
        assert!(cmds.contains(&"node.open_current"));
        assert!(cmds.contains(&"node.delete"));
    }

    #[test]
    fn process_row_menu_has_copy_actions() {
        use super::process_row_menu;
        let items = process_row_menu(true);
        let cmds = commands(&items);
        assert!(cmds.contains(&"process.copy_pid"));
        assert!(cmds.contains(&"process.copy_name"));
        assert!(cmds.contains(&"process.copy_path"));
        // Copy PID enabled when the row has a PID.
        assert!(items.iter().any(|it| matches!(it,
            MenuItem::Action { command, enabled, .. }
                if command == "process.copy_pid" && *enabled)));
    }

    #[test]
    fn process_row_menu_copy_pid_disabled_without_pid() {
        use super::process_row_menu;
        // A synthetic (non-process) row has no PID → "Copy PID" disabled.
        let items = process_row_menu(false);
        assert!(items.iter().any(|it| matches!(it,
            MenuItem::Action { command, enabled, .. }
                if command == "process.copy_pid" && !*enabled)));
    }
}
