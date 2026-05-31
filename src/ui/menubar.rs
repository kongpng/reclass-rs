//! In-window menu bar — the frameless titlebar menu row (app-shell §7).
//!
//! Port of the C++ `createMenus` / titlebar menu row (`titlebar.cpp`): the
//! horizontal "File · Edit · View · Tools · Plugins · Help" bar where each
//! top-level title opens a dropdown of commands. The original mirrored the live
//! `QMenuBar` into Linux tool-buttons; here each top-level title is a ghost
//! [`Button`] trigger and the dropdown is a Zed-styled elevated surface of inset
//! rows (hover overlay, separators, left checkmark slot, right keybinding hint).
//!
//! ## Why this is NOT a [`Popover`](gpui_component::popover::Popover)
//! The menu bar lives **inside** the frameless [`TitleBar`](gpui_component::TitleBar),
//! whose whole strip is a window-**drag** region (it grabs `on_mouse_down` to
//! start a window move). gpui-component's own in-titlebar menu bar
//! (`menu/app_menu_bar.rs::AppMenu`) does NOT use `Popover` for exactly this
//! reason — a `Popover`'s trigger does not `stop_propagation` on mouse-down, so
//! the titlebar swallows the click and the dropdown never opens (the visual-QA
//! "View did not open any dropdown" bug). We mirror the proven `AppMenu` recipe:
//! a [`Button`] trigger that **stops propagation on mouse-down** (defeating the
//! drag) and toggles an explicit open-index on click, with the dropdown rendered
//! as a `deferred(anchored(...))` child guarded by that index. The menu bar owns
//! the open state ([`MenuBar::open_index`]) so only one menu is open at a time and
//! hovering a sibling title switches to it (the C++ menu-bar behaviour).
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
use gpui_component::{Selectable as _, Sizable as _};

use super::commandpalette::{default_menu_tree, CommandId, MenuNode};
use super::design::{color, elevated_surface, icon, tokens};

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
    /// The index of the currently-open top-level menu (`None` ⇒ all closed). The
    /// C++ `QMenuBar` opens one menu at a time; clicking a title toggles it,
    /// hovering a sibling *while one is open* switches to it, and choosing a
    /// command (or clicking out / Escape) closes them all.
    open_index: Option<usize>,
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
            open_index: None,
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

    /// Toggle the top-level menu at `index` open/closed (the C++ title click). If
    /// a *different* menu is open it switches to this one; clicking the open menu's
    /// title again closes it.
    fn toggle_menu(&mut self, index: usize, cx: &mut Context<Self>) {
        self.open_index = if self.open_index == Some(index) {
            None
        } else {
            Some(index)
        };
        cx.notify();
    }

    /// Switch the open menu to `index` *only while a menu is already open* — the
    /// C++ "slide across the bar with the mouse" behaviour. A no-op when nothing
    /// is open (so a passive hover never pops a menu).
    fn hover_menu(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.open_index.is_some() && self.open_index != Some(index) {
            self.open_index = Some(index);
            cx.notify();
        }
    }

    /// Close all menus (a command was chosen, the user clicked out, or Escape).
    fn close_menus(&mut self, cx: &mut Context<Self>) {
        if self.open_index.take().is_some() {
            cx.notify();
        }
    }

    /// Emit a chosen command to the host (the C++ `action->trigger()` →
    /// `MainWindow` slot) and close the menus. Called from a dropdown item's click.
    fn choose_command(&mut self, command: CommandId, cx: &mut Context<Self>) {
        self.open_index = None;
        cx.emit(MenuCommand(command));
        cx.notify();
    }
}

/// Strip a `&` mnemonic from a top-level title (the menu tree carries `&File`
/// etc. for the palette's mnemonic handling; the bar shows the bare word).
fn clean_title(label: &str) -> String {
    label.replace('&', "")
}

impl Render for MenuBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // One top-level menu per submenu, in declaration order
        // (File · Edit · View · Tools · Plugins · Help).
        let menus = self.menus.clone();
        let open_index = self.open_index;
        // Snapshot the checked-command set once here (where `self` is borrowed) so
        // the dropdown builders don't re-`read` this same entity mid-render.
        let checked = self.checked.clone();

        gpui_component::h_flex()
            .id("rcx-menubar")
            .flex_none()
            .items_center()
            .gap(px(tokens::space::XXS))
            .children(menus.into_iter().enumerate().filter_map(move |(i, node)| {
                match node {
                    MenuNode::Submenu { label, children } => Some(render_top_level(
                        i,
                        clean_title(&label),
                        children,
                        open_index == Some(i),
                        checked.clone(),
                        cx,
                    )),
                    // Top-level leaves/separators don't appear in the C++ bar.
                    _ => None,
                }
            }))
    }
}

/// Build one top-level menu: a ghost [`Button`] title that toggles this menu's
/// open state on click, plus — while `open` — a `deferred(anchored(...))` dropdown
/// of the submenu's children anchored below-left of the title.
///
/// The trigger mirrors gpui-component's in-titlebar `AppMenu` (`app_menu_bar.rs`):
/// it **stops propagation on mouse-down** so the surrounding [`TitleBar`] drag
/// region never swallows the click, then toggles the menu on click. Hovering a
/// title while *another* menu is open switches to it (the C++ menu-bar slide).
fn render_top_level(
    index: usize,
    title: String,
    children: Vec<MenuNode>,
    open: bool,
    checked: HashSet<CommandId>,
    cx: &mut Context<MenuBar>,
) -> AnyElement {
    // The clickable title — Zed chrome: muted text, hover overlay, MD radius. It
    // selects (lightens) while its dropdown is open (the C++ pressed-title look).
    let trigger = Button::new(("menu-title", index))
        .ghost()
        .small()
        .compact()
        .label(title)
        .selected(open)
        .text_color(color::text_muted(cx))
        // Stop the mouse-down from reaching the TitleBar's window-drag handler —
        // this is the fix for "clicking a menu title did nothing" (the title was
        // being consumed as the start of a window move). Mirrors `AppMenu`.
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.prevent_default();
            cx.stop_propagation();
        })
        .on_click(cx.listener(move |this, _ev, _window, cx| {
            this.toggle_menu(index, cx);
        }));

    div()
        .id(("menu", index))
        .relative()
        .child(trigger)
        // Slide-across: hovering a title while another menu is open switches to it.
        .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
            if *hovered {
                this.hover_menu(index, cx);
            }
        }))
        .when(open, |this| {
            this.child(deferred(
                anchored()
                    .anchor(gpui::Anchor::TopLeft)
                    .snap_to_window_with_margin(px(tokens::space::MD))
                    .child(
                        div()
                            .occlude()
                            .top(px(tokens::space::XS))
                            // Dismiss when the user clicks anywhere outside the
                            // dropdown (the C++ menu loses focus → closes).
                            .on_mouse_down_out(cx.listener(|this, _ev, _window, cx| {
                                this.close_menus(cx);
                            }))
                            .child(menu_dropdown(&children, &checked, cx)),
                    ),
            ))
        })
        .into_any_element()
}

/// The dropdown body for a submenu's `children`: an elevated surface of inset
/// rows. Leaves are clickable command rows (checkmark slot · label · shortcut
/// hint); separators are 1px rules; nested submenus collapse to a labelled
/// section header with their leaves inlined one indent in, so nothing is silently
/// dropped (the bar must always OPEN and show items).
fn menu_dropdown(
    children: &[MenuNode],
    checked: &HashSet<CommandId>,
    cx: &mut Context<MenuBar>,
) -> impl IntoElement {
    // The host's checked-command set (the C++ checkable `QAction` state — e.g.
    // `view.scanner` while the scanner pop-out is open) is threaded down from
    // `render` so every row's checkmark stays in lockstep with the live app state
    // without re-`read`ing this entity mid-render.
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
                    cx,
                )
                .into_any_element(),
            ),
            MenuNode::Submenu {
                label,
                children: sub,
            } => {
                // Render the submenu header as a section label, then inline its
                // leaf children one indent in — the chrome surface keeps the bar
                // flat (no fly-out submenus) but shows every command.
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
                                cx,
                            )
                            .into_any_element(),
                        );
                    }
                }
            }
        }
    }

    elevated_surface(cx)
        .occlude()
        .min_w(px(220.0))
        .p(px(tokens::space::XS))
        .child(gpui_component::v_flex().gap(px(1.0)).children(rows))
}

/// A clickable command row: `[checkmark slot] Label …………… [Shortcut]`.
/// Hover overlay, `MD` radius, disabled rows greyed + inert (the C++ menu shows
/// disabled actions). When `checked`, the leading slot shows a real SVG check
/// (the Assets-stage [`icon::check`]) — the C++ checkable `QAction` (e.g. View ▸
/// Memory Scanner while the scanner pop-out is open). Clicking emits
/// [`MenuCommand`] (via [`MenuBar::choose_command`]) and closes the menu.
fn command_row(
    key: usize,
    label: &str,
    shortcut: &str,
    command: CommandId,
    enabled: bool,
    checked: bool,
    cx: &mut Context<MenuBar>,
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
            r.cursor_pointer()
                .hover(|s| s.bg(color::hover_overlay(cx)))
                .on_click(cx.listener(move |this, _ev, _window, cx| {
                    cx.stop_propagation();
                    this.choose_command(command.clone(), cx);
                }))
        })
        .child(check_slot)
        .child(div().flex_1().min_w_0().child(label.to_string()))
        .when_some(hint, |r, h| r.child(h))
}

/// A 1px separator rule between menu groups (spec §5.10).
fn separator_row(cx: &Context<MenuBar>) -> impl IntoElement {
    div()
        .my(px(tokens::space::XS))
        .h(px(tokens::border::THIN))
        .w_full()
        .bg(color::border(cx))
}

/// A nested-submenu header row (e.g. "Import", "Export") — a small uppercase
/// muted caption introducing the inlined leaves below it.
fn submenu_header_row(label: &str, cx: &Context<MenuBar>) -> impl IntoElement {
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
