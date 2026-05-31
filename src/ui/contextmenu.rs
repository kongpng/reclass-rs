//! Context menus — the node / hex-byte / workspace right-click menus
//! (`editor.cpp` `customContextMenuRequested`, `main.cpp` workspace-tree menu,
//! widgets-dialogs.md §0 "context menus").
//!
//! Port of the right-click menu *contents* + their selection-dependent logic.
//! Per the cookbook (ARCHITECTURE §5) the rendering maps onto
//! `ContextMenuExt` + `PopupMenu`; the gpui side needs an `Action` per item, so
//! this module models the menus as **data** ([`MenuItem`] catalogues, keyed by a
//! [`CommandId`](super::commandpalette::CommandId)) + the load-bearing logic
//! (single-vs-multi selection, class↔struct convert, pin/unpin label, delete
//! label pluralization) — all unit-tested — and the host turns the item list into
//! a `PopupMenu`.
//!
//! Gated behind the `ui` feature only for the (small) menu-builder helper; the
//! catalogues + logic are always built/tested.

use super::commandpalette::CommandId;

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
}
