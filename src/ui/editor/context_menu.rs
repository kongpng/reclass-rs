//! The editor's right-click context menus — the node / member / batch /
//! empty-area menus plus the show/close plumbing — extracted from the oversized
//! editor/mod.rs into a second `impl RcxEditor`. As a child module of `editor` it
//! keeps full access to RcxEditor's private fields and methods.

use super::*;
use crate::controller::Modifiers as CtrlMods;
use crate::core::linemeta::K_COMMAND_ROW_ID;
use crate::core::{LineKind, NodeKind};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::IconName;

/// Append the always-available Fold ▸ / Copy ▸ / Tracking ▸ submenus the C++ adds
/// after every with-node menu (controller.cpp:3880-3974). Shared by the enum-header
/// and enum/bitfield-member menus so they carry the same trailing block as the
/// struct menu. (The struct menu inlines its own copy because its Fold submenu also
/// offers the per-node Collapse/Expand.)
fn append_node_submenus(
    menu: gpui_component::menu::PopupMenu,
    mw: &mut Window,
    mcx: &mut Context<gpui_component::menu::PopupMenu>,
    track_values: bool,
) -> gpui_component::menu::PopupMenu {
    menu.submenu("Fold", mw, mcx, |sub, _w, _cx| {
        sub.menu_with_icon(
            "Collapse All",
            IconName::ChevronRight,
            Box::new(EditorCollapseAll),
        )
        .menu_with_icon(
            "Expand All",
            IconName::ChevronDown,
            Box::new(EditorExpandAll),
        )
    })
    .submenu("Copy", mw, mcx, |sub, _w, _cx| {
        sub.menu_with_icon("Copy Address", IconName::Copy, Box::new(EditorCopyAddress))
            .menu_with_icon("Copy Offset", IconName::Copy, Box::new(EditorCopyOffset))
            .separator()
            .menu_with_icon("Copy Line", IconName::Copy, Box::new(EditorCopyLine))
            .menu_with_icon(
                "Copy All as Text",
                IconName::Copy,
                Box::new(EditorCopyAllText),
            )
    })
    .submenu("Tracking", mw, mcx, move |sub, _w, _cx| {
        sub.menu_with_check(
            "Track Value Changes",
            track_values,
            Box::new(EditorTrackToggle),
        )
        .menu_with_icon(
            "Clear All History",
            IconName::Delete,
            Box::new(EditorTrackClear),
        )
    })
}

/// Part D: prepend the "Selected bytes (N) ▸" submenu to a node/batch menu when a
/// byte selection is active (the C++ `addByteSubmenu`, controller.cpp:231). Mirrors
/// `addByteSubmenu`'s item order: Copy as hex / Copy as C array / Copy as Python
/// bytes / Edit hex / Zero-fill / Paste hex / Save bytes as binary / Break into new
/// class, then a separator. No-op when `byte_active` is false. Paste/Zero-fill are
/// shown unconditionally (the controller's handlers re-check writability and emit a
/// "read-only" hint), matching the C++ which lists them always.
fn add_byte_submenu(
    menu: gpui_component::menu::PopupMenu,
    byte_active: bool,
    byte_count: i32,
    mw: &mut Window,
    mcx: &mut Context<gpui_component::menu::PopupMenu>,
) -> gpui_component::menu::PopupMenu {
    if !byte_active {
        return menu;
    }
    menu.submenu(
        SharedString::from(format!("Selected bytes ({byte_count})")),
        mw,
        mcx,
        |sub, _w, _cx| {
            sub.menu_with_icon("Copy as hex", IconName::Copy, Box::new(EditorByteCopyHex))
                .menu_with_icon(
                    "Copy as C array",
                    IconName::Copy,
                    Box::new(EditorByteCopyCArray),
                )
                .menu_with_icon(
                    "Copy as Python bytes",
                    IconName::Copy,
                    Box::new(EditorByteCopyPython),
                )
                .menu_with_icon("Edit hex\u{2026}", IconName::Replace, Box::new(EditorByteEditHex))
                .menu_with_icon("Zero-fill", IconName::Minus, Box::new(EditorByteZeroFill))
                .menu_with_icon("Paste hex", IconName::Inbox, Box::new(EditorBytePasteHex))
                .menu_with_icon(
                    "Save bytes as binary\u{2026}",
                    IconName::HardDrive,
                    Box::new(EditorByteSaveBinary),
                )
                .menu_with_icon(
                    "Break into new class",
                    IconName::Frame,
                    Box::new(EditorByteBreakClass),
                )
        },
    )
    .separator()
}

impl super::RcxEditor {
    // ── Node context menu (reclass `customContextMenuRequested`) ──

    /// Right-mouse-down entry point invoked by [`RowElement`] (window-space
    /// position). Records the right-clicked row as the menu's [`ContextTarget`],
    /// commits any active edit, ensures the node is selected, then opens the Zed
    /// [`PopupMenu`](gpui_component::menu::PopupMenu) anchored at the cursor
    /// (reclass_right_click_on_address.png). Rows with no real node (command row /
    /// footer / synthetic) do not open a node menu.
    pub(crate) fn dispatch_row_context_menu(
        &mut self,
        line: usize,
        _rel_x: f32,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing.is_some() {
            self.commit_active_edit(window, cx);
        }
        let Some(lm) = self.line_meta(line).cloned() else {
            return;
        };
        // Item 6 / B4: the command-row header gets the no-node menu (Insert 4 / 8
        // bytes, Append bytes, Fold / Copy / Tracking). "Convert
        // to Struct" / "Convert to Class" is NOT decided here — it is bound to a
        // dedicated shaped-position hitbox over the `struct`/`class` KEYWORD
        // (`rcx-keyword-hover`), which opens the convert menu directly. Routing it
        // there (instead of by `rel_x` → column here) is what makes the option
        // appear ONLY when the keyword is right-clicked: a right-click on the name /
        // source chip / gaps reaches this branch, and the command row's `▸`/`▾`/chip
        // glyph drift would otherwise pull the painted name's column back into the
        // keyword span and convert by mistake.
        if lm.line_kind == LineKind::CommandRow && lm.node_id == K_COMMAND_ROW_ID {
            self.context_target = None;
            self.open_empty_area_menu(pos, window, cx);
            return;
        }
        // Only real node rows get the node menu (command/footer/synthetic rows
        // have their own affordances and no node ops). Item 17: an empty-area /
        // no-node row — including FOOTER rows — gets the C++ no-node menu (Insert 4
        // / Insert 8 / Append bytes…); the keyboard Insert actions already append at
        // the view root when there is no current node, so the menu rows reuse them.
        if lm.node_idx < 0 || lm.node_id == 0 || lm.node_id == K_COMMAND_ROW_ID {
            self.context_target = None;
            self.open_empty_area_menu(pos, window, cx);
            return;
        }
        let target = ContextTarget {
            line,
            node_idx: lm.node_idx as usize,
            node_id: lm.node_id,
            kind: lm.node_kind,
            sub_line: lm.sub_line,
        };
        self.context_target = Some(target);

        // Right-click selects the node (single-select) if it is not already part
        // of the selection — matches the reclass behaviour where the menu acts on
        // the clicked node. Membership uses the SAME encoded id the click path stores
        // (`sel_id_for_line`): array-element / member / footer rows carry encoding
        // bits in `sel_ids`, so a raw-node-id test would treat an already-selected
        // such row as "outside" and reset it — wrongly dropping an active byte
        // selection (and its submenu) when you right-click the rows it covers
        // (the C++ `showContextMenu` selection policy, controller.cpp:3925).
        let clicked_id = crate::core::sel_id_for_line(&lm);
        let already_selected = self.controller.selected_ids().contains(&clicked_id);

        // Item 7/22/46: when MORE THAN ONE node is selected and the right-clicked
        // node is part of that selection, open the BATCH menu (Change to <type> for
        // N nodes, Group into Union, Delete N nodes, …) instead of the single-node
        // menu — and do NOT collapse the multi-selection to the clicked node.
        let sel_count = self.controller.selected_ids().len();
        if sel_count > 1 && already_selected {
            // C++ only offers "Group into Union" when every selected node shares a
            // parent (controller.cpp:3216-3229); a cross-parent selection hides it
            // rather than showing a clickable item that silently no-ops.
            let same_parent = {
                let tree = self.controller.tree();
                let mut parents = self.controller.selected_ids().iter().map(|&id| {
                    let idx = tree.index_of_id(crate::controller::strip_sel_pub(id));
                    if idx >= 0 {
                        tree.nodes[idx as usize].parent_id
                    } else {
                        0
                    }
                });
                match parents.next() {
                    Some(first) => parents.all(|p| p == first),
                    None => true,
                }
            };
            self.open_batch_context_menu(sel_count, same_parent, pos, window, cx);
            return;
        }

        if !already_selected {
            // Right-clicking a row outside the current selection moves the selection
            // here and drops any active byte selection (and its submenu) so the two
            // stay coherent. clear_byte_selection mirrors an empty covered set (→
            // clears sel_ids), so do it before installing the clicked id.
            if self.byte_sel.is_active() {
                self.clear_byte_selection();
            }
            self.controller
                .handle_node_click(line as i64, lm.node_id, CtrlMods::NONE);
            let _ = self.controller.take_events();
        }

        // Item 6: a right-click on an enum / bitfield MEMBER row opens the
        // member-specific menu (Add Member Above/Below + Remove Member for enum
        // members; Toggle Bit for bitfield members) ahead of the always-available
        // node actions. Resolve member-ness from the row's `is_member_line` +
        // `sub_line` against the node's enum/bitfield kind.
        if lm.is_member_line && target.sub_line >= 0 {
            let (is_enum_member, is_bitfield_member, bit_width) = {
                let tree = self.controller.tree();
                match tree.nodes.get(target.node_idx) {
                    Some(n) => {
                        let sl = target.sub_line as usize;
                        let is_bf = n.is_bitfield() && sl < n.bitfield_members.len();
                        // Item 9: the member's bit width — Toggle Bit is offered ONLY
                        // for a single-bit member (bitWidth == 1); a multi-bit member
                        // gets Edit Value… instead (the C++ `bm.bitWidth == 1`
                        // branch, controller.cpp:3350).
                        let bw = if is_bf {
                            n.bitfield_members[sl].bit_width
                        } else {
                            0
                        };
                        (n.is_enum() && sl < n.enum_members.len(), is_bf, bw)
                    }
                    None => (false, false, 0),
                }
            };
            if is_enum_member || is_bitfield_member {
                self.open_member_context_menu(
                    target,
                    is_enum_member,
                    is_bitfield_member,
                    bit_width,
                    pos,
                    window,
                    cx,
                );
                return;
            }
        }

        // A right-click on an enum HEADER row (not a member line) opens a RESTRICTED
        // menu — Add Member / Rename / Delete + the shared Fold/Copy/Tracking — rather
        // than the full struct menu, whose type-cycler / Convert / Insert / Big-endian
        // etc. would corrupt the enum (the C++ enum-header branch, controller.cpp:3362).
        // An enum is a NodeKind::Struct, so it otherwise falls through to the container
        // menu and (for an empty enum) offers no way to add the first member.
        if self.node_is_enum(target.node_idx) {
            self.open_enum_header_context_menu(target, pos, window, cx);
            return;
        }

        self.open_context_menu(target, pos, window, cx);
    }

    /// The enum HEADER context menu (the C++ enum-header branch, controller.cpp:3362):
    /// Add Member / Rename / Delete, then the shared Fold/Copy/Tracking submenus.
    /// Deliberately OMITS the struct ops (type-cycler, Change Type, Insert, Convert,
    /// Structure, Big endian, Duplicate, Copy-as-C-Struct) that would corrupt an enum.
    fn open_enum_header_context_menu(
        &mut self,
        _target: ContextTarget,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor_focus = self.focus_handle.clone();
        let track_values = self.controller.track_values();
        let menu = gpui_component::menu::PopupMenu::build(window, cx, move |menu, mw, mcx| {
            let menu = menu
                .min_w(px(200.0))
                .action_context(editor_focus.clone())
                .menu_with_icon("Add Member", IconName::Plus, Box::new(EditorEnumAddMember))
                .separator()
                .menu_with_icon("Rename", IconName::SquareTerminal, Box::new(EditorRename))
                .menu_with_icon("Delete", IconName::Delete, Box::new(EditorDelete))
                .separator();
            append_node_submenus(menu, mw, mcx, track_values)
        });
        self.show_context_menu_at(menu, pos, window, cx);
    }

    /// Item 6: the enum / bitfield MEMBER row context menu (the C++
    /// `showContextMenu` member-line branch, controller.cpp:3315). Enum members get
    /// Add Member Above / Add Member Below / Remove Member; bitfield members get
    /// Toggle Bit. Both fall through to Edit Value (the always-available member
    /// edit). Wired to the existing controller member mutators.
    fn open_member_context_menu(
        &mut self,
        // The member-menu actions read `self.context_target` (set by the caller), so
        // the menu items only need the enum/bitfield flags; the target is implicit.
        _target: ContextTarget,
        is_enum_member: bool,
        is_bitfield_member: bool,
        // Item 9: the bitfield member's bit width — gates Toggle Bit (width == 1) vs
        // Edit Value… (multi-bit). Unused for enum members.
        bit_width: u8,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor_focus = self.focus_handle.clone();
        let track_values = self.controller.track_values();
        let menu = gpui_component::menu::PopupMenu::build(window, cx, move |menu, mw, mcx| {
            let mut menu = menu.min_w(px(200.0)).action_context(editor_focus.clone());
            if is_enum_member {
                menu = menu
                    .menu_with_icon(
                        "Add Member Above",
                        IconName::Plus,
                        Box::new(EditorMemberAddAbove),
                    )
                    .menu_with_icon(
                        "Add Member Below",
                        IconName::Plus,
                        Box::new(EditorMemberAddBelow),
                    )
                    .menu_with_icon(
                        "Remove Member",
                        IconName::Delete,
                        Box::new(EditorMemberRemove),
                    );
                // NOTE: the C++ enum-member branch does NOT add an Edit Value item
                // (controller.cpp:3315-3361) — inline edit still covers it. Removed.
            }
            if is_bitfield_member {
                // Item 9: Toggle Bit ONLY for a single-bit member; a multi-bit member
                // gets Edit Value… instead (mutually exclusive — the C++
                // `bm.bitWidth == 1 ? "Toggle Bit" : "Edit Value..."`,
                // controller.cpp:3350). The C++ does NOT gate this on writability.
                if bit_width == 1 {
                    menu = menu.menu_with_icon(
                        "Toggle Bit",
                        IconName::Check,
                        Box::new(EditorMemberToggleBit),
                    );
                } else {
                    menu = menu.menu_with_icon(
                        "Edit Value...",
                        IconName::SquareTerminal,
                        Box::new(EditorBeginValueEdit),
                    );
                }
            }
            // The C++ member branch falls through to the always-available
            // Fold/Copy/Tracking submenus (controller.cpp:3315 has no early return)
            // — append them so a member row gets the same trailing block as a node.
            append_node_submenus(menu.separator(), mw, mcx, track_values)
        });
        self.show_context_menu_at(menu, pos, window, cx);
    }

    /// Item 7/18/22/46/48: the multi-selection BATCH context menu (the C++
    /// `showContextMenu` `selCount > 1` branch). Offers Change to <type>… (the quick
    /// type-cycler applied to every selected node), Group into Union (controller
    /// `group_into_union`), Insert Above, Duplicate N, Delete N, and Copy Address.
    /// Reuses the existing batch-aware controller mutators (`quick_change_kind`,
    /// `action_duplicate`, `action_delete` already iterate the whole selection).
    fn open_batch_context_menu(
        &mut self,
        count: usize,
        same_parent: bool,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor_focus = self.focus_handle.clone();
        let show_comment = self.show_comments();
        // Part D: byte submenu (prepended) + bottom "Clear selection".
        let byte_active = self.byte_sel.is_active();
        let byte_count = self.byte_sel.len() as i32;
        let menu = gpui_component::menu::PopupMenu::build(window, cx, move |menu, mw, mcx| {
            let menu = menu.min_w(px(220.0)).action_context(editor_focus.clone());
            let menu = add_byte_submenu(menu, byte_active, byte_count, mw, mcx);
            menu
                // "Change type of N nodes…" → opens the Change Type picker (applies
                // to every selected node via the batch-aware controller path).
                .menu_with_icon(
                    SharedString::from(format!("Change type of {count} nodes\u{2026}")),
                    IconName::Frame,
                    Box::new(EditorChangeType),
                )
                // Quick "Change to <hexN>" rows the C++ batch menu lists.
                .submenu("Change to", mw, mcx, |sub, _w, _cx| {
                    sub.menu_with_icon("Hex8", IconName::Frame, Box::new(EditorHex8))
                        .menu_with_icon("Hex16", IconName::Frame, Box::new(EditorHex16))
                        .menu_with_icon("Hex32", IconName::Frame, Box::new(EditorHex32))
                        .menu_with_icon("Hex64", IconName::Frame, Box::new(EditorHex64))
                        .menu_with_icon(
                            "Pointer",
                            IconName::ArrowRight,
                            Box::new(EditorQuickPointer),
                        )
                })
                .separator()
                // Item 18/48: Group into Union — wraps the selected nodes into a
                // union (controller `group_into_union`). Only when all selected nodes
                // share a parent (controller.cpp:3216), else the item is hidden.
                .when(same_parent, |menu| {
                    menu.menu_with_icon(
                        "Group into Union",
                        IconName::Frame,
                        Box::new(EditorGroupIntoUnion),
                    )
                })
                .menu_with_icon("Insert Above", IconName::Plus, Box::new(EditorInsertAbove))
                .separator()
                .when(show_comment, |menu| {
                    menu.menu_with_icon(
                        SharedString::from(format!("Comment {count} nodes")),
                        IconName::SquareTerminal,
                        Box::new(EditorCommentEdit),
                    )
                })
                .menu_with_icon(
                    SharedString::from(format!("Duplicate {count} nodes")),
                    IconName::Copy,
                    Box::new(EditorDuplicate),
                )
                .menu_with_icon(
                    SharedString::from(format!("Delete {count} nodes")),
                    IconName::Delete,
                    Box::new(EditorDelete),
                )
                .separator()
                .submenu("Copy", mw, mcx, |sub, _w, _cx| {
                    sub.menu_with_icon("Copy Address", IconName::Copy, Box::new(EditorCopyAddress))
                })
                // Part D: bottom "Clear selection" — clears byte + row selection
                // together (the batch branch always has a multi-selection).
                .separator()
                .menu_with_icon(
                    "Clear selection",
                    IconName::Close,
                    Box::new(EditorClearSelection),
                )
        });
        self.show_context_menu_at(menu, pos, window, cx);
    }

    /// Item 11/17: the no-node (empty area) context menu (the C++ `!hasNode`
    /// branch, controller.cpp:3882). Insert ▸ (Insert 4 / Insert 8 / Append bytes…),
    /// then the
    /// always-appended Fold / Copy / Tracking submenus the C++ adds after the
    /// hasNode/!hasNode split (controller.cpp:3913-3974). The empty-area Copy has no
    /// Address/Offset group (no node) — only Copy Line / Copy All as Text.
    fn open_empty_area_menu(
        &mut self,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor_focus = self.focus_handle.clone();
        let track_values = self.controller.track_values();
        let menu = gpui_component::menu::PopupMenu::build(window, cx, move |menu, mw, mcx| {
            menu.min_w(px(200.0))
                .action_context(editor_focus.clone())
                .submenu("Insert", mw, mcx, |sub, _w, _cx| {
                    sub.menu_with_icon("Insert 4", IconName::Plus, Box::new(EditorInsertHex32))
                        .menu_with_icon("Insert 8", IconName::Plus, Box::new(EditorInsertHex64))
                        .separator()
                        .menu_with_icon(
                            "Append bytes…",
                            IconName::Plus,
                            Box::new(EditorAppendBytes),
                        )
                })
                .separator()
                // Fold ▸ — Collapse All / Expand All (whole tree).
                .submenu("Fold", mw, mcx, |sub, _w, _cx| {
                    sub.menu_with_icon(
                        "Collapse All",
                        IconName::ChevronRight,
                        Box::new(EditorCollapseAll),
                    )
                    .menu_with_icon(
                        "Expand All",
                        IconName::ChevronDown,
                        Box::new(EditorExpandAll),
                    )
                })
                // Copy ▸ — Copy Line / Copy All as Text (no node ⇒ no Address/Offset).
                .submenu("Copy", mw, mcx, |sub, _w, _cx| {
                    sub.menu_with_icon("Copy Line", IconName::Copy, Box::new(EditorCopyLine))
                        .menu_with_icon(
                            "Copy All as Text",
                            IconName::Copy,
                            Box::new(EditorCopyAllText),
                        )
                })
                // Tracking ▸ — Track Value Changes (checkable) / Clear All History.
                .submenu("Tracking", mw, mcx, move |sub, _w, _cx| {
                    sub.menu_with_check(
                        "Track Value Changes",
                        track_values,
                        Box::new(EditorTrackToggle),
                    )
                    .menu_with_icon(
                        "Clear All History",
                        IconName::Delete,
                        Box::new(EditorTrackClear),
                    )
                })
        });
        self.show_context_menu_at(menu, pos, window, cx);
    }

    /// Build + show the node context menu at `pos`. Each item dispatches one of
    /// the `Editor*` actions (handled on this view), so menu-click and the bound
    /// accelerators share one code path. Item layout mirrors the C++ menu
    /// (reclass_right_click_on_address.png): New Class · Ptr to New Class · the
    /// `← cur ↔ alt →` quick type-cycler · Rename · Change Type · Insert ▸ ·
    /// Convert ▸ · Big endian · Structure ▸ · Duplicate · Delete · Fold ▸ · Copy ▸ ·
    /// Tracking ▸ · Copy as C Struct, with leading SVG icons + accelerator hints.
    fn open_context_menu(
        &mut self,
        target: ContextTarget,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cur_name = crate::core::kind_to_string(target.kind);
        let alt_name = crate::core::kind_to_string(alt_kind_for(target.kind));
        let prev_name = crate::core::kind_to_string(prev_kind_for(target.kind));
        // Two directional quick-cycler rows — forward (`→ next`) and backward
        // (`← prev`) — mirroring the Left/Right keyboard cycler. C++ exposes the
        // cycler only via the arrow keys; the Rust menu rows are an affordance on
        // top, now symmetric so the menu can step the type either way (P7).
        let cycle_next_label = format!("{cur_name}  \u{2192}  {alt_name}");
        let cycle_prev_label = format!("{cur_name}  \u{2190}  {prev_name}");
        let is_container = crate::core::is_container_kind(target.kind);
        // Item 14: label the Fold entry by the container's live collapsed state —
        // 'Expand' when collapsed, 'Collapse' when expanded (the C++ `&Expand` /
        // `&Collapse`). State-agnostic 'Toggle Fold' before.
        let fold_collapsed = {
            let tree = self.controller.tree();
            let idx = tree.index_of_id(target.node_id);
            idx >= 0 && tree.nodes[idx as usize].collapsed
        };
        let fold_label = if fold_collapsed { "Expand" } else { "Collapse" };
        // Item 7/9: hex-node-only menu entries (Edit Bytes / Split to hexN) + the
        // node's live big-endian state for the checkable toggle (item 13).
        let is_hex_ctx = is_hex_preview(target.kind);
        let big_endian = {
            let idx = self.controller.tree().index_of_id(target.node_id);
            idx >= 0 && self.controller.tree().nodes[idx as usize].big_endian
        };
        // ── Item 11: inference-based quick-convert (the C++ `Convert to <type>` /
        // `Split into <type>xN`, controller.cpp:3471) ──
        //   For a HEX node, read the row's TypeHint chip `type_hint_kinds` (populated
        //   by compose). A single hint → "Convert to <name>" (changeNodeKind); a
        //   multi-kind hint → "Split into <name>xN". The dynamic kind(s) are stashed
        //   in `pending_hint_convert` so the parameterless menu actions can read them
        //   on click.
        let hint_kinds: Vec<NodeKind> = if is_hex_ctx {
            self.line_meta(target.line)
                .and_then(|lm| {
                    lm.chips
                        .iter()
                        .find(|c| c.kind == crate::core::linemeta::ChipKind::TypeHint)
                        .map(|c| c.type_hint_kinds.clone())
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let hint_label: Option<String> = match hint_kinds.len() {
            0 => None,
            1 => Some(format!(
                "Convert to {}",
                crate::core::kind_to_string(hint_kinds[0])
            )),
            n => Some(format!(
                "Split into {}\u{00D7}{}",
                crate::core::kind_to_string(hint_kinds[0]),
                n
            )),
        };
        let hint_is_split = hint_kinds.len() > 1;
        // Stash for the action handlers (cleared by them via `.take()`).
        self.pending_hint_convert = if hint_kinds.is_empty() {
            None
        } else {
            Some((target.node_id, hint_kinds))
        };
        // ── Menu-item gates (items 16/17/42), computed against the C++ rules ──
        let byte_size = crate::core::size_for_kind(target.kind);
        // Item 16: New Class only for NON-container kinds; Ptr to New Class also
        // requires a 4- or 8-byte node; Rename omitted for hex nodes; Big endian
        // only for scalar numeric kinds.
        let show_new_class = !is_container;
        let show_ptr_new_class = !is_container && (byte_size == 4 || byte_size == 8);
        let show_rename = !is_hex_ctx;
        let show_big_endian = is_scalar_numeric_kind(target.kind);
        // Item 17: Edit Value for writable, non-hex, non-container nodes; Comment
        // only when the Comments toggle is on.
        let writable = self.provider_writable();
        let show_edit_value = writable && !is_hex_ctx && !is_container;
        let show_comment = self.show_comments();
        // ── Item 9: Convert-submenu gates, computed by kind (controller.cpp:3631) ──
        use crate::core::NodeKind as NK;
        let k = target.kind;
        let conv_uint_label = match k {
            NK::Hex64 => Some("uint64_t\tU"),
            NK::Hex32 => Some("uint32_t\tU"),
            _ => None,
        };
        let conv_float_label = match k {
            NK::Hex64 => Some("double\tF"),
            NK::Hex32 => Some("float\tF"),
            _ => None,
        };
        // Hex16 → int16_t (S). (No uint/float quick-row for hex16 in C++.)
        let conv_int16 = matches!(k, NK::Hex16);
        // ptr\tP only when size >= 4.
        let conv_ptr = byte_size >= 4;
        let conv_fnptr64 = matches!(k, NK::Hex64 | NK::Pointer64);
        let conv_fnptr32 = matches!(k, NK::Hex32 | NK::Pointer32);
        let conv_ptr64_back = matches!(k, NK::FuncPtr64);
        let conv_ptr32_back = matches!(k, NK::FuncPtr32);
        // "Change to ptr*" — typed pointer, for 4/8-byte non-container nodes that
        // are not ALREADY a typed pointer.
        let already_typed_ptr = {
            let idx = self.controller.tree().index_of_id(target.node_id);
            idx >= 0
                && matches!(k, NK::Pointer32 | NK::Pointer64)
                && self.controller.tree().nodes[idx as usize].ref_id != 0
        };
        let conv_ptr_star =
            (byte_size == 4 || byte_size == 8) && !is_container && !already_typed_ptr;
        // Per-size Split labels (Hex128→hex64+hex64 … Hex16→hex8+hex8).
        let conv_split_label = match k {
            NK::Hex128 => Some("Split to hex64+hex64"),
            NK::Hex64 => Some("Split to hex32+hex32"),
            NK::Hex32 => Some("Split to hex16+hex16"),
            NK::Hex16 => Some("Split to hex8+hex8"),
            _ => None,
        };
        // Convert to Hex only for non-hex non-container.
        let conv_to_hex = !is_hex_ctx && !is_container;
        // Disable the whole submenu when no conversion applies.
        let convert_enabled = conv_uint_label.is_some()
            || conv_float_label.is_some()
            || conv_int16
            || conv_ptr
            || conv_fnptr64
            || conv_fnptr32
            || conv_ptr64_back
            || conv_ptr32_back
            || conv_ptr_star
            || conv_split_label.is_some()
            || conv_to_hex;
        // Gap 20: the live value-change tracking flag (drives the Tracking submenu
        // check). Read once here so the menu closure can capture it by value.
        let track_values = self.controller.track_values();

        // ── Item 8: Structure-submenu gates (the C++ `Structure` submenu,
        // controller.cpp:3782) ──
        //   * Add Child (Hex64): container (Struct/Array) heads.
        //   * Dissolve Union: the node is a union, or its parent is a union.
        let (static_add_child, static_dissolve_union) = {
            let tree = self.controller.tree();
            match tree.nodes.get(target.node_idx) {
                Some(n) => {
                    let is_container_node = matches!(n.kind, NodeKind::Struct | NodeKind::Array);
                    let dissolve = if n.kind == NodeKind::Struct && n.is_union() {
                        true
                    } else if n.parent_id != 0 {
                        tree.nodes
                            .get(tree.index_of_id(n.parent_id).max(0) as usize)
                            .map(|p| p.kind == NodeKind::Struct && p.is_union())
                            .unwrap_or(false)
                    } else {
                        false
                    };
                    (is_container_node, dissolve)
                }
                None => (false, false),
            }
        };
        let static_has_any = static_add_child || static_dissolve_union;

        // Part D: the live byte selection drives the "Selected bytes (N) ▸" submenu
        // prepended at the top, and the bottom "Clear selection" (shown when there's
        // a byte selection OR a row selection).
        let byte_active = self.byte_sel.is_active();
        let byte_count = self.byte_sel.len() as i32;
        let show_clear = byte_active || !self.controller.selected_ids().is_empty();

        let editor_focus = self.focus_handle.clone();
        let menu = gpui_component::menu::PopupMenu::build(window, cx, move |menu, mw, mcx| {
            let menu = menu
                .min_w(px(220.0))
                // Dispatch the menu's actions to the editor's focus context (the
                // `RcxEditor` key context that registers the `Editor*` handlers).
                .action_context(editor_focus.clone());
            // Prepend the "Selected bytes (N) ▸" submenu (+ separator) when active.
            let menu = add_byte_submenu(menu, byte_active, byte_count, mw, mcx);
            menu
                // Item 16: New Class only for non-container kinds; Ptr to New Class
                // additionally requires a 4/8-byte node.
                .when(show_new_class, |menu| {
                    menu.menu_with_icon("New Class", IconName::Frame, Box::new(EditorNewClass))
                })
                .when(show_ptr_new_class, |menu| {
                    menu.menu_with_icon(
                        "Ptr to New Class",
                        IconName::ArrowRight,
                        Box::new(EditorPtrToNewClass),
                    )
                })
                .separator()
                // The quick type-cycler rows: `cur → next` steps the node's kind
                // forward, `cur ← prev` steps it back (the C++ in-place type stepper
                // bound to Right/Left). Two rows so both directions are reachable by
                // mouse, not just the keyboard.
                .menu_with_icon(
                    cycle_next_label,
                    IconName::ChevronDown,
                    Box::new(EditorCycleTypeNext),
                )
                .menu_with_icon(
                    cycle_prev_label,
                    IconName::ChevronUp,
                    Box::new(EditorCycleTypePrev),
                )
                .separator()
                // Item 17: Edit Value (Enter) for writable, non-hex, non-container.
                .when(show_edit_value, |menu| {
                    menu.menu_with_icon(
                        "Edit Value",
                        IconName::SquareTerminal,
                        Box::new(EditorBeginValueEdit),
                    )
                })
                // Item 16/19: Rename omitted for hex nodes; F2 hint appended.
                .when(show_rename, |menu| {
                    menu.menu_with_icon("Rename", IconName::SquareTerminal, Box::new(EditorRename))
                })
                .menu_with_icon("Change Type", IconName::Frame, Box::new(EditorChangeType))
                // Item 17: Comment (;) only when the Comments toggle is on.
                .when(show_comment, |menu| {
                    menu.menu_with_icon(
                        "Comment",
                        IconName::SquareTerminal,
                        Box::new(EditorCommentEdit),
                    )
                })
                .separator()
                // Item 15: the C++ Insert submenu offers Insert 4 Above (Hex32,
                // Shift+Ins) / Insert 8 Above (Hex64, Ins) — the keyboard already
                // maps Insert/Shift+Insert to those — plus Insert Below.
                .submenu("Insert", mw, mcx, |sub, _w, _cx| {
                    sub.menu_with_icon(
                        "Insert 8 Above (Hex64)",
                        IconName::Plus,
                        Box::new(EditorInsertHex64),
                    )
                    .menu_with_icon(
                        "Insert 4 Above (Hex32)",
                        IconName::Plus,
                        Box::new(EditorInsertHex32),
                    )
                    .separator()
                    .menu_with_icon("Insert Below", IconName::Plus, Box::new(EditorInsertBelow))
                    .menu_with_icon("Insert Above", IconName::Plus, Box::new(EditorInsertAbove))
                    // Item 17: the C++ Insert submenu ends with "Append bytes…".
                    .separator()
                    .menu_with_icon(
                        "Append bytes…",
                        IconName::Plus,
                        Box::new(EditorAppendBytes),
                    )
                })
                // Item 9: the C++ Convert submenu — SIZE-SPECIFIC labels by kind
                // (controller.cpp:3631), only the applicable rows, with U/F/S/P
                // hints, fnptr/ptr toggles, per-size Split labels, Convert-to-Hex
                // gating, and the whole submenu disabled when nothing applies.
                .when(!convert_enabled, |menu| {
                    menu.submenu("Convert", mw, mcx, |sub, _w, _cx| {
                        sub.label("(no conversion)")
                    })
                })
                .when(convert_enabled, |menu| {
                    menu.submenu("Convert", mw, mcx, move |mut sub, _w, _cx| {
                        if let Some(lbl) = conv_uint_label {
                            sub =
                                sub.menu_with_icon(lbl, IconName::Frame, Box::new(EditorConvUInt));
                        }
                        if let Some(lbl) = conv_float_label {
                            sub =
                                sub.menu_with_icon(lbl, IconName::Frame, Box::new(EditorConvFloat));
                        }
                        if conv_int16 {
                            sub = sub.menu_with_icon(
                                "int16_t\tS",
                                IconName::Frame,
                                Box::new(EditorConvInt),
                            );
                        }
                        if conv_ptr {
                            // Size-aware pointer (P key path: 8→Pointer64, else 32).
                            sub = sub.menu_with_icon(
                                "ptr\tP",
                                IconName::ArrowRight,
                                Box::new(EditorQuickPointer),
                            );
                        }
                        if conv_fnptr64 {
                            sub = sub.menu_with_icon(
                                "fnptr64",
                                IconName::SquareTerminal,
                                Box::new(EditorConvFnPtr64),
                            );
                        }
                        if conv_fnptr32 {
                            sub = sub.menu_with_icon(
                                "fnptr32",
                                IconName::SquareTerminal,
                                Box::new(EditorConvFnPtr32),
                            );
                        }
                        if conv_ptr64_back {
                            sub = sub.menu_with_icon(
                                "ptr64",
                                IconName::ArrowRight,
                                Box::new(EditorConvPtr64),
                            );
                        }
                        if conv_ptr32_back {
                            sub = sub.menu_with_icon(
                                "ptr32",
                                IconName::ArrowRight,
                                Box::new(EditorConvPtr32),
                            );
                        }
                        if conv_ptr_star {
                            sub = sub.separator().menu_with_icon(
                                "Change to ptr*",
                                IconName::ArrowRight,
                                Box::new(EditorConvertPtr),
                            );
                        }
                        if let Some(lbl) = conv_split_label {
                            sub = sub.menu_with_icon(
                                lbl,
                                IconName::Frame,
                                Box::new(EditorConvSplitHex),
                            );
                        }
                        if conv_to_hex {
                            sub = sub.menu_with_icon(
                                "Convert to Hex",
                                IconName::Frame,
                                Box::new(EditorConvHex),
                            );
                        }
                        sub
                    })
                })
                // Item 11: inference-based quick-convert — "Convert to <type>"
                // (single hint) or "Split into <type>xN" (multi). Shown only for a
                // hex node with a TypeHint (the C++ `lm.typeHintKinds` block,
                // controller.cpp:3471), with a trailing separator.
                .when(hint_label.is_some(), |menu| {
                    let label = SharedString::from(hint_label.clone().unwrap_or_default());
                    let action: Box<dyn gpui::Action> = if hint_is_split {
                        Box::new(EditorHintSplit)
                    } else {
                        Box::new(EditorHintConvert)
                    };
                    menu.menu_with_icon(label, IconName::Frame, action)
                        .separator()
                })
                // Item 16: Big endian only for scalar numeric kinds.
                .when(show_big_endian, |menu| {
                    menu.menu_with_check("Big endian", big_endian, Box::new(EditorToggleBigEndian))
                })
                // Item 7/42: in-place hex / ASCII overwrite editor entry points,
                // shown for hex nodes — but ONLY when the provider is writable (a
                // read-only/File-backed source must not offer in-place byte edits).
                .when(is_hex_ctx && writable, |menu| {
                    menu.menu_with_icon(
                        "Edit Bytes (Hex)",
                        IconName::SquareTerminal,
                        Box::new(EditorEditBytesHex),
                    )
                    .menu_with_icon(
                        "Edit ASCII",
                        IconName::SquareTerminal,
                        Box::new(EditorEditBytesAscii),
                    )
                })
                // Item 8: the Structure submenu — real entries wired to the
                // controller's add-child / dissolve-union mutators (the C++
                // `Structure` submenu, controller.cpp:3782). Shown only when at
                // least one entry applies; a placeholder otherwise.
                .submenu("Structure", mw, mcx, move |mut sub, _w, _cx| {
                    if !static_has_any {
                        return sub.label("(none)");
                    }
                    if static_add_child {
                        sub = sub.menu_with_icon(
                            "Add Child",
                            IconName::Plus,
                            Box::new(EditorStaticAddChild),
                        );
                    }
                    if static_dissolve_union {
                        sub = sub.menu_with_icon(
                            "Dissolve Union",
                            IconName::Frame,
                            Box::new(EditorStaticDissolveUnion),
                        );
                    }
                    sub
                })
                .separator()
                .menu_with_icon("Duplicate", IconName::Copy, Box::new(EditorDuplicate))
                .menu_with_icon("Delete", IconName::Delete, Box::new(EditorDelete))
                .separator()
                // Item 18: Fold submenu — Toggle Fold + Collapse All / Expand All
                // (whole-tree), with the keyboard hints the C++ uses.
                .submenu("Fold", mw, mcx, move |sub, _w, _cx| {
                    // Item 14: 'Expand' / 'Collapse' per the container's state.
                    sub.menu_with_icon_and_disabled(
                        fold_label,
                        IconName::ChevronRight,
                        Box::new(EditorFold),
                        !is_container,
                    )
                    .separator()
                    .menu_with_icon(
                        "Collapse All",
                        IconName::ChevronRight,
                        Box::new(EditorCollapseAll),
                    )
                    .menu_with_icon(
                        "Expand All",
                        IconName::ChevronDown,
                        Box::new(EditorExpandAll),
                    )
                })
                // Item 17/19: the Copy submenu — Copy Address / Offset · Line / All
                // as Text, with a separator between the address/offset group and the
                // line/all group (the C++ separator), plus Ctrl+C/Ctrl+X hints.
                .submenu("Copy", mw, mcx, |sub, _w, _cx| {
                    sub.menu_with_icon("Copy Address", IconName::Copy, Box::new(EditorCopyAddress))
                        .menu_with_icon("Copy Offset", IconName::Copy, Box::new(EditorCopyOffset))
                        .separator()
                        .menu_with_icon("Copy Line", IconName::Copy, Box::new(EditorCopyLine))
                        .menu_with_icon(
                            "Copy All as Text",
                            IconName::Copy,
                            Box::new(EditorCopyAllText),
                        )
                })
                .submenu("Tracking", mw, mcx, move |sub, _w, _cx| {
                    // Gap 20: live value-change tracking toggle + clear-history. The
                    // controller already owns `set_track_values` / `reset_change_
                    // tracking`; these wire them (was a dead "(tracking)" label). The
                    // check reflects the current `track_values` flag.
                    sub.menu_with_check(
                        "Track Value Changes",
                        track_values,
                        Box::new(EditorTrackToggle),
                    )
                    .menu_with_icon(
                        "Clear All History",
                        IconName::Delete,
                        Box::new(EditorTrackClear),
                    )
                })
                .menu_with_icon(
                    "Copy as C Struct",
                    IconName::SquareTerminal,
                    Box::new(EditorCopyCStruct),
                )
                // Part D: bottom "Clear selection" — clears the byte selection AND
                // the row selection together (the C++ menu tail). Only offered when
                // there's something to clear.
                .when(show_clear, |menu| {
                    menu.separator().menu_with_icon(
                        "Clear selection",
                        IconName::Close,
                        Box::new(EditorClearSelection),
                    )
                })
        });

        self.show_context_menu_at(menu, pos, window, cx);
    }

    /// Mount a built [`PopupMenu`](gpui_component::menu::PopupMenu) at `pos`: wire
    /// its dismiss subscription, record it, and keep editor focus so dispatched
    /// actions land in the `RcxEditor` key context. Shared by the node menu and the
    /// command-row root-keyword convert menu (item 6).
    pub(super) fn show_context_menu_at(
        &mut self,
        menu: Entity<gpui_component::menu::PopupMenu>,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Dismiss (click-away / Esc on the menu) closes + clears state.
        self._context_menu_sub =
            Some(cx.subscribe(&menu, |this, _menu, _ev: &DismissEvent, cx| {
                this.context_menu = None;
                this._context_menu_sub = None;
                cx.notify();
            }));
        self.context_menu_pos = pos;
        // Dismiss any live hover card the instant the menu opens so it does not
        // linger behind / over the menu (the row under a right-click usually has a
        // hover card pending from the move that preceded the click).
        self.hover_popup = None;
        // Focus the MENU (not the editor) so the keyboard drives it: Up/Down move
        // the highlight, Enter activates the highlighted item, and Esc closes the
        // menu — instead of Esc falling through to the editor and clearing the row
        // selection behind it. Every editor menu is built with
        // `.action_context(editor_focus)`, so a chosen action still dispatches into
        // the `RcxEditor` key context, and dismissing the menu (Esc / click-away /
        // pick) restores editor focus via `PopupMenu::dismiss` (popup_menu.rs:959).
        let menu_focus = menu.focus_handle(cx);
        self.context_menu = Some(menu);
        window.focus(&menu_focus, cx);
        cx.notify();
    }

    /// Close the context menu (if any) and drop its subscription.
    pub(super) fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() {
            self._context_menu_sub = None;
            cx.notify();
        }
    }
}
