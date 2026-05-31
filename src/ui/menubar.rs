//! In-window menu bar — the frameless titlebar menu row (app-shell §7).
//!
//! Port of the C++ `createMenus` / titlebar menu row (`titlebar.cpp`): the
//! horizontal "File · Edit · View · Tools · Plugins · Help" bar where each
//! top-level title opens a dropdown of commands. The original mirrored the live
//! `QMenuBar` into Linux tool-buttons; here each top-level title is a ghost
//! [`Button`] trigger wrapped in a gpui-component [`Popover`], and the dropdown
//! is a Zed-styled elevated surface of inset rows (hover overlay, separators,
//! left checkmark slot, right keybinding hint).
//!
//! ## Reuse, not re-model
//! The command/menu tree already exists as data for the command palette
//! ([`crate::ui::commandpalette::default_menu_tree`] →
//! [`MenuNode`](crate::ui::commandpalette::MenuNode)). This module **reuses** it
//! verbatim — every dropdown is rendered straight from those [`MenuNode`]s, so
//! the menu bar and the palette can never drift. Leaf items carry a
//! [`CommandId`](crate::ui::commandpalette::CommandId); clicking one emits
//! [`MenuCommand`] which [`MainWindow`](crate::ui::window::MainWindow) routes to
//! `run_menu_command` (the same handler the palette's `Trigger` uses).
//!
//! Follows the shared Zed design system ([`crate::ui::design`]; spec §5.2,
//! §5.10): chrome-bg titles in `text_muted`, hover overlay, `MD` radius on the
//! title, dropdowns on `elevated_surface` with `LG` radius + a soft shadow.
//!
//! Gated behind the `ui` feature.

use std::collections::HashSet;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::popover::{Popover, PopoverState};
use gpui_component::Sizable as _;

use super::commandpalette::{default_menu_tree, CommandId, MenuNode};
use super::design::{color, icon, tokens};

/// The menu-bar's outcome — a top-level→leaf command was chosen. Routed by the
/// owning [`MainWindow`](super::window::MainWindow) to `run_menu_command` (the
/// same dispatch the command palette's `Trigger` uses).
#[derive(Clone, Debug)]
pub struct MenuCommand(pub CommandId);

/// The in-window menu bar view — a horizontal row of top-level menu titles, each
/// opening a Zed-styled dropdown built from the shared menu tree.
///
/// Owned by [`MainWindow`](super::window::MainWindow), which subscribes to
/// [`MenuCommand`] and dispatches the chosen command.
pub struct MenuBar {
    /// The menu tree (the shared palette tree by default). Held so the dropdowns
    /// render from the exact same data the command palette searches.
    menus: Vec<MenuNode>,
    /// Command ids that should render a leading checkmark (checkable/toggle menu
    /// items reflecting live app state — e.g. `view.scanner` while the scanner
    /// pop-out is open; the C++ `QAction::setChecked`). The host
    /// ([`MainWindow`](super::window::MainWindow)) updates this via
    /// [`set_command_checked`](Self::set_command_checked).
    checked: HashSet<CommandId>,
}

impl EventEmitter<MenuCommand> for MenuBar {}

impl MenuBar {
    /// Build the menu bar over the default Reclass menu tree (app-shell §7).
    pub fn new(_cx: &mut Context<Self>) -> Self {
        MenuBar {
            menus: default_menu_tree(),
            checked: HashSet::new(),
        }
    }

    /// Construct as an [`Entity`] (the form the titlebar/window holds).
    pub fn view(cx: &mut App) -> Entity<Self> {
        cx.new(Self::new)
    }

    /// Set whether a command's menu item renders a leading checkmark (the C++
    /// checkable `QAction`). The host calls this to reflect live toggle state —
    /// e.g. `view.scanner` checked while the scanner pop-out is open. Re-renders
    /// only when the state actually changed.
    pub fn set_command_checked(&mut self, command: &str, checked: bool, cx: &mut Context<Self>) {
        let changed = if checked {
            self.checked.insert(command.to_string())
        } else {
            self.checked.remove(command)
        };
        if changed {
            cx.notify();
        }
    }

    /// Emit a chosen command to the host (the C++ `action->trigger()` →
    /// `MainWindow` slot). Called from a dropdown item's click.
    fn emit_command(&mut self, command: CommandId, cx: &mut Context<Self>) {
        cx.emit(MenuCommand(command));
    }
}

/// Strip a `&` mnemonic from a top-level title (the menu tree carries `&File`
/// etc. for the palette's mnemonic handling; the bar shows the bare word).
fn clean_title(label: &str) -> String {
    label.replace('&', "")
}

impl Render for MenuBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // One dropdown per top-level submenu, in declaration order
        // (File · Edit · View · Tools · Plugins · Help).
        let this = cx.entity();
        let menus = self.menus.clone();

        gpui_component::h_flex()
            .id("rcx-menubar")
            .flex_none()
            .items_center()
            .gap(px(tokens::space::XXS))
            .children(
                menus
                    .into_iter()
                    .enumerate()
                    .filter_map(move |(i, node)| match node {
                        MenuNode::Submenu { label, children } => Some(top_level_menu(
                            i,
                            clean_title(&label),
                            children,
                            this.clone(),
                            cx,
                        )),
                        // Top-level leaves/separators don't appear in the C++ bar.
                        _ => None,
                    }),
            )
    }
}

/// Build one top-level menu: a ghost [`Button`] trigger that opens a [`Popover`]
/// dropdown of the submenu's children. The dropdown content is rebuilt from the
/// [`MenuNode`] children every render (cheap; the cookbook's Popover guidance).
fn top_level_menu(
    index: usize,
    title: String,
    children: Vec<MenuNode>,
    menubar: Entity<MenuBar>,
    cx: &App,
) -> impl IntoElement {
    // The clickable title — Zed chrome: muted text, hover overlay, MD radius.
    // It selects (lightens) while its popover is open (Popover wires that via
    // `Selectable`).
    let trigger = Button::new(("menu-title", index))
        .ghost()
        .small()
        .label(title)
        .text_color(color::text_muted(cx));

    // The dropdown anchors below-left of the trigger — `Anchor::TopLeft` is the
    // Popover default, so no explicit anchor is needed.
    Popover::new(("menu", index))
        .trigger(trigger)
        .content(move |_state, _window, cx| {
            // Rebuild the dropdown body from the submenu children. `cx.entity()`
            // is the `PopoverState`, captured so item clicks can dismiss it.
            let popover = cx.entity();
            menu_dropdown(&children, menubar.clone(), popover, cx)
        })
}

/// The dropdown body for a submenu's `children`: an elevated surface of inset
/// rows. Leaves are clickable command rows (checkmark slot · label · shortcut
/// hint); separators are 1px rules; empty submenus (e.g. "Recent Files") and
/// nested submenus collapse to a single labelled, non-interactive parent row so
/// nothing is silently dropped (the bar must always OPEN and show items).
fn menu_dropdown(
    children: &[MenuNode],
    menubar: Entity<MenuBar>,
    popover: Entity<PopoverState>,
    cx: &mut Context<PopoverState>,
) -> impl IntoElement {
    // Snapshot the host's checked-command set once (the C++ checkable `QAction`
    // state — e.g. `view.scanner` while the scanner pop-out is open). Read here so
    // every row's checkmark stays in lockstep with the live app state.
    let checked = menubar.read(cx).checked.clone();
    let is_checked = |command: &str| checked.contains(command);

    let mut rows: Vec<AnyElement> = Vec::new();
    for (i, child) in children.iter().enumerate() {
        match child {
            MenuNode::Separator => rows.push(separator_row(cx).into_any_element()),
            MenuNode::Item {
                label,
                shortcut,
                command,
                enabled,
            } => rows.push(
                command_row(
                    i,
                    label,
                    shortcut,
                    command.clone(),
                    *enabled,
                    is_checked(command),
                    menubar.clone(),
                    popover.clone(),
                    cx,
                )
                .into_any_element(),
            ),
            MenuNode::Submenu {
                label,
                children: sub,
            } => {
                // Render the submenu header as a (disabled-looking) section label,
                // then inline its leaf children one indent in — the chrome surface
                // keeps the bar flat (no fly-out submenus) but shows every command.
                rows.push(submenu_header_row(label, cx).into_any_element());
                for (j, leaf) in sub.iter().enumerate() {
                    if let MenuNode::Item {
                        label,
                        shortcut,
                        command,
                        enabled,
                    } = leaf
                    {
                        rows.push(
                            command_row(
                                1000 + i * 100 + j,
                                label,
                                shortcut,
                                command.clone(),
                                *enabled,
                                is_checked(command),
                                menubar.clone(),
                                popover.clone(),
                                cx,
                            )
                            .into_any_element(),
                        );
                    }
                }
            }
        }
    }

    gpui_component::v_flex()
        .min_w(px(220.0))
        .p(px(tokens::space::XS))
        .gap(px(1.0))
        .children(rows)
}

/// A clickable command row: `[checkmark slot] Label …………… [Shortcut]`.
/// Hover overlay, `MD` radius, disabled rows greyed + inert (the C++ menu shows
/// disabled actions). When `checked`, the leading slot shows a real SVG check
/// (the Assets-stage [`icon::check`]) — the C++ checkable `QAction` (e.g. View ▸
/// Memory Scanner while the scanner pop-out is open). Clicking emits
/// [`MenuCommand`] and dismisses the popover.
#[allow(clippy::too_many_arguments)]
fn command_row(
    key: usize,
    label: &str,
    shortcut: &str,
    command: CommandId,
    enabled: bool,
    checked: bool,
    menubar: Entity<MenuBar>,
    popover: Entity<PopoverState>,
    cx: &Context<PopoverState>,
) -> impl IntoElement {
    let label_color = if enabled {
        color::text(cx)
    } else {
        color::text_disabled(cx)
    };
    let muted = color::text_muted(cx);
    let accent = color::accent(cx);

    // Leading fixed-width checkmark slot — keeps every label left-aligned whether
    // or not a row is checkable. Holds a real SVG check (accent-tinted) when this
    // command's toggle is on; empty (but space-reserving) otherwise.
    let check_slot = div()
        .flex_none()
        .w(px(tokens::font::UI_MD))
        .h(px(tokens::font::UI_MD))
        .flex()
        .items_center()
        .justify_center()
        .when(checked, |s| {
            s.text_color(accent).child(icon::check().xsmall())
        });

    // Right-aligned keybinding hint, "Ctrl+S" → "Ctrl S" muted text (Zed shows a
    // light hint, not key-caps, in menus).
    let hint: Option<AnyElement> = if shortcut.is_empty() {
        None
    } else {
        Some(
            div()
                .flex_none()
                .text_size(px(tokens::font::UI_XS))
                .text_color(muted)
                .child(shortcut.replace('+', " "))
                .into_any_element(),
        )
    };

    gpui_component::h_flex()
        .id(("menu-item", key))
        .w_full()
        .h(px(26.0))
        .px(px(tokens::space::SM))
        .gap(px(tokens::space::SM))
        .items_center()
        .justify_between()
        .rounded(px(tokens::radius::MD))
        .text_size(px(tokens::font::UI_MD))
        .text_color(label_color)
        .when(enabled, |r| {
            let menubar = menubar.clone();
            let popover = popover.clone();
            r.cursor_pointer()
                .hover(|s| s.bg(color::hover_overlay(cx)))
                .on_click(move |_e, window, cx| {
                    cx.stop_propagation();
                    // Emit the command to the host, then dismiss the dropdown.
                    let command = command.clone();
                    menubar.update(cx, |mb, cx| mb.emit_command(command, cx));
                    popover.update(cx, |state, cx| state.dismiss(window, cx));
                })
        })
        .child(check_slot)
        .child(div().flex_1().min_w_0().child(label.to_string()))
        .when_some(hint, |r, h| r.child(h))
}

/// A 1px separator rule between menu groups (spec §5.10).
fn separator_row(cx: &Context<PopoverState>) -> impl IntoElement {
    div()
        .my(px(tokens::space::XS))
        .h(px(tokens::border::THIN))
        .w_full()
        .bg(color::border(cx))
}

/// A nested-submenu header row (e.g. "Import", "Export") — a small uppercase
/// muted caption introducing the inlined leaves below it.
fn submenu_header_row(label: &str, cx: &Context<PopoverState>) -> impl IntoElement {
    div()
        .w_full()
        .px(px(tokens::space::MD))
        .pt(px(tokens::space::SM))
        .pb(px(tokens::space::XXS))
        .text_size(px(tokens::font::UI_XS))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color::text_muted(cx))
        .child(clean_title(label).to_uppercase())
}

#[cfg(test)]
mod tests {
    // Only the gpui-free helpers are unit-tested headlessly (importing `super::*`
    // would pull the module's `gpui::*` glob into the test hygiene expansion; see
    // the titlebar.rs note).
    use super::clean_title;

    #[test]
    fn clean_title_strips_mnemonic_ampersand() {
        assert_eq!(clean_title("&File"), "File");
        assert_eq!(clean_title("&Edit"), "Edit");
        assert_eq!(clean_title("Help"), "Help");
        // Only `&` is stripped; the rest is untouched.
        assert_eq!(clean_title("&Save As…"), "Save As…");
    }
}
