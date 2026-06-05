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
    /// The currently-open cascading submenu path *within* the open top-level menu
    /// — a chain of child indices (the C++ `QMenu` opening a nested `QMenu` as the
    /// pointer rests on a "▸" item). Empty ⇒ no fly-out open. Hovering a different
    /// submenu row at a given depth replaces the tail (the classic "slide between
    /// fly-outs" behaviour); hovering a plain leaf collapses any deeper fly-outs.
    open_submenu: Vec<usize>,
    /// Keyboard-navigation highlight: the row index (in the DEEPEST open panel)
    /// that ↑/↓ moved to and Enter activates. `None` until the first arrow key, and
    /// cleared whenever the mouse changes the open structure (so pointer and
    /// keyboard don't fight over the highlight).
    highlight: Option<usize>,
    /// Command ids that should render a leading checkmark (checkable/toggle menu
    /// items reflecting live app state — e.g. `view.scanner` while the scanner
    /// pop-out is open; the C++ `QAction::setChecked`). The host
    /// ([`MainWindow`](super::window::MainWindow)) updates this via
    /// [`set_command_checked`](Self::set_command_checked).
    checked: HashSet<CommandId>,
    /// Whether the top-level menu titles render upper-cased (the C++
    /// `menuBarTitleCase` ⇒ `applyMenuBarTitleCase(true)`; main.cpp:1063). `false`
    /// (the C++ default) renders them Title-Cased. The host
    /// ([`MainWindow`](super::window::MainWindow)) pushes the persisted preference
    /// via [`set_title_case`](Self::set_title_case).
    title_case: bool,
    /// Focus handle the open dropdown holds while a menu is up, so the keyboard
    /// (Escape, bound on the dropdown) can dismiss it. A C++ `QMenu` runs a modal
    /// loop that catches Escape; here the dropdown is a `deferred` overlay, so it
    /// must explicitly take focus to receive keys.
    dropdown_focus: FocusHandle,
    /// Who held focus when the menu opened — restored on Escape / click-out so the
    /// editor keeps the keyboard after the menu is *cancelled* (a chosen command
    /// instead lets its own handler decide focus).
    restore_focus: Option<WeakFocusHandle>,
}

impl EventEmitter<MenuCommand> for MenuBar {}

impl MenuBar {
    /// Build the menu bar over the default Reclass menu tree (app-shell §7).
    pub fn new(cx: &mut Context<Self>) -> Self {
        MenuBar {
            menus: default_menu_tree(),
            open_index: None,
            open_submenu: Vec::new(),
            highlight: None,
            checked: HashSet::new(),
            // The C++ default is Title-Case (`menuBarTitleCase = false`).
            title_case: false,
            dropdown_focus: cx.focus_handle(),
            restore_focus: None,
        }
    }

    /// Set whether the top-level titles render upper-cased (the C++
    /// `applyMenuBarTitleCase`; main.cpp:1063). `true` ⇒ `FILE EDIT VIEW …`;
    /// `false` ⇒ `File Edit View …`. Re-renders only when the value changes.
    pub fn set_title_case(&mut self, title_case: bool, cx: &mut Context<Self>) {
        if self.title_case != title_case {
            self.title_case = title_case;
            cx.notify();
        }
    }

    /// Construct as an [`Entity`] (the form the titlebar/window holds).
    pub fn view(cx: &mut App) -> Entity<Self> {
        cx.new(Self::new)
    }

    /// Replace the whole menu tree (the C++ menus rebuilt on `aboutToShow` for
    /// the dynamic Recent-Files / Data-Source submenus and the dynamic MCP
    /// Start/Stop label). The host builds the tree from live data via
    /// [`menu_tree_with`](crate::ui::commandpalette::menu_tree_with) and pushes it
    /// here; always re-renders so the next open shows the fresh rows.
    pub fn set_menus(&mut self, menus: Vec<MenuNode>, cx: &mut Context<Self>) {
        self.menus = menus;
        cx.notify();
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
    /// title again closes it. Always collapses any open fly-out chain. `pub` so the
    /// host window can drive it from the Alt+letter menu mnemonics.
    pub fn toggle_menu(&mut self, index: usize, cx: &mut Context<Self>) {
        self.open_index = if self.open_index == Some(index) {
            None
        } else {
            Some(index)
        };
        self.open_submenu.clear();
        self.highlight = None;
        cx.notify();
    }

    /// Toggle the top-level menu whose `&`-mnemonic matches `letter`
    /// (case-insensitive) — the Alt+letter accelerator (`Alt+F` ⇒ `&File`). Returns
    /// whether a menu matched. Resolved by scanning the live menu labels so it
    /// tracks the menu order rather than hard-coding indices.
    pub fn activate_mnemonic(&mut self, letter: char, cx: &mut Context<Self>) -> bool {
        let want = letter.to_ascii_lowercase();
        let idx = self.menus.iter().position(|n| match n {
            MenuNode::Submenu { label, .. } => mnemonic_of(label) == Some(want),
            _ => false,
        });
        match idx {
            Some(i) => {
                self.toggle_menu(i, cx);
                true
            }
            None => false,
        }
    }

    /// Switch the open menu to `index` *only while a menu is already open* — the
    /// C++ "slide across the bar with the mouse" behaviour. A no-op when nothing
    /// is open (so a passive hover never pops a menu). Collapses fly-outs from the
    /// previously-open menu.
    fn hover_menu(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.open_index.is_some() && self.open_index != Some(index) {
            self.open_index = Some(index);
            self.open_submenu.clear();
            self.highlight = None;
            cx.notify();
        }
    }

    /// Open the cascading fly-out for the submenu reached by `path` (a chain of
    /// child indices from the open top-level menu). Hovering a "▸" row calls this;
    /// it replaces the current fly-out chain (so sliding from one submenu row to a
    /// sibling swaps the open fly-out). No-op if already open at that exact path.
    fn open_submenu_path(&mut self, path: Vec<usize>, cx: &mut Context<Self>) {
        if self.open_submenu != path {
            self.open_submenu = path;
            // A mouse hover changed the open fly-out — drop the keyboard highlight.
            self.highlight = None;
            cx.notify();
        }
    }

    /// Collapse any fly-out at `depth` and deeper (the pointer moved onto a plain
    /// leaf at this level, so its sibling fly-outs must close). Keeps the shallower
    /// part of the chain intact. No-op if nothing deeper is open.
    fn collapse_submenu_to(&mut self, depth: usize, cx: &mut Context<Self>) {
        if self.open_submenu.len() > depth {
            self.open_submenu.truncate(depth);
            cx.notify();
        }
    }

    /// Close all menus (a command was chosen, the user clicked out, or Escape).
    fn close_menus(&mut self, cx: &mut Context<Self>) {
        let was_open = self.open_index.take().is_some() || !self.open_submenu.is_empty();
        self.open_submenu.clear();
        self.highlight = None;
        if was_open {
            cx.notify();
        }
    }

    // ── Keyboard navigation of the open menu (↑/↓ move the highlight, Enter
    // activates, Tab / ← / → traverse). The highlight always refers to the DEEPEST
    // open panel; mouse hover clears it so the two input modes don't fight. ──

    /// The children of the deepest currently-open panel (the top-level menu's body,
    /// or the body of the innermost open fly-out). Empty when no menu is open.
    fn deepest_children(&self) -> Vec<MenuNode> {
        let Some(top) = self.open_index else {
            return Vec::new();
        };
        let mut cur = match self.menus.get(top) {
            Some(MenuNode::Submenu { children, .. }) => children,
            _ => return Vec::new(),
        };
        for &idx in &self.open_submenu {
            match cur.get(idx) {
                Some(MenuNode::Submenu { children, .. }) => cur = children,
                _ => break,
            }
        }
        cur.clone()
    }

    /// Whether a row can hold the keyboard highlight (enabled leaf or a submenu;
    /// separators and disabled rows are skipped).
    fn selectable(node: &MenuNode) -> bool {
        matches!(
            node,
            MenuNode::Item { enabled: true, .. } | MenuNode::Submenu { .. }
        )
    }

    /// Move the highlight by `delta` over the selectable rows of the deepest panel,
    /// wrapping. From no highlight, ↓ lands on the first row and ↑ on the last.
    fn move_highlight(&mut self, delta: i32, cx: &mut Context<Self>) {
        let rows = self.deepest_children();
        let sel: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, n)| Self::selectable(n))
            .map(|(i, _)| i)
            .collect();
        if sel.is_empty() {
            return;
        }
        let pos = self.highlight.and_then(|h| sel.iter().position(|&i| i == h));
        let next = match pos {
            Some(p) => (p as i32 + delta).rem_euclid(sel.len() as i32) as usize,
            None if delta >= 0 => 0,
            None => sel.len() - 1,
        };
        self.highlight = Some(sel[next]);
        cx.notify();
    }

    /// Activate the highlighted row: an enabled leaf runs its command (closing the
    /// menu); a submenu opens its fly-out and moves the highlight into it.
    fn activate_highlight(&mut self, cx: &mut Context<Self>) {
        let Some(h) = self.highlight else { return };
        match self.deepest_children().get(h) {
            Some(MenuNode::Item {
                command,
                enabled: true,
                ..
            }) => {
                let command = command.clone();
                self.choose_command(command, cx);
            }
            Some(MenuNode::Submenu { .. }) => self.open_highlighted_submenu(cx),
            _ => {}
        }
    }

    /// Open the highlighted submenu's fly-out (keyboard → / Enter) and move the
    /// highlight to the first selectable row of the new panel.
    fn open_highlighted_submenu(&mut self, cx: &mut Context<Self>) {
        let Some(h) = self.highlight else { return };
        if matches!(self.deepest_children().get(h), Some(MenuNode::Submenu { .. })) {
            self.open_submenu.push(h);
            self.highlight = None;
            self.move_highlight(1, cx); // first selectable row of the fly-out
        }
    }

    /// Keyboard ←: collapse the innermost open fly-out (re-highlighting the parent
    /// row), or — at the top level — switch to the previous top-level menu.
    fn back_or_prev_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(parent) = self.open_submenu.pop() {
            self.highlight = Some(parent);
            cx.notify();
        } else {
            self.switch_menu(-1, cx);
        }
    }

    /// Keyboard →: open the highlighted submenu, or — on a leaf — move to the next
    /// top-level menu (the C++ Right-arrow menu-bar walk).
    fn right_key(&mut self, cx: &mut Context<Self>) {
        let is_submenu = self
            .highlight
            .map(|h| matches!(self.deepest_children().get(h), Some(MenuNode::Submenu { .. })))
            .unwrap_or(false);
        if is_submenu {
            self.open_highlighted_submenu(cx);
        } else {
            self.switch_menu(1, cx);
        }
    }

    /// Switch the open top-level menu by `delta` (Tab / Shift+Tab, ←/→ at the top
    /// level), wrapping; resets the fly-out chain and highlight.
    fn switch_menu(&mut self, delta: i32, cx: &mut Context<Self>) {
        let tops: Vec<usize> = self
            .menus
            .iter()
            .enumerate()
            .filter(|(_, n)| matches!(n, MenuNode::Submenu { .. }))
            .map(|(i, _)| i)
            .collect();
        if tops.is_empty() {
            return;
        }
        let cur = self.open_index.unwrap_or(tops[0]);
        let pos = tops.iter().position(|&i| i == cur).unwrap_or(0);
        let next = (pos as i32 + delta).rem_euclid(tops.len() as i32) as usize;
        self.open_index = Some(tops[next]);
        self.open_submenu.clear();
        self.highlight = None;
        cx.notify();
    }

    /// Close all menus and hand the keyboard back to whoever held it when the menu
    /// opened (Escape / click-outside — the *cancel* paths). The editor would
    /// otherwise be left with no focused element, since the dropdown's focus handle
    /// stops rendering the moment it closes.
    fn close_menus_restoring(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menus(cx);
        if let Some(handle) = self.restore_focus.take().and_then(|w| w.upgrade()) {
            window.focus(&handle, cx);
        }
    }

    /// Emit a chosen command to the host (the C++ `action->trigger()` →
    /// `MainWindow` slot) and close the menus. Called from a dropdown item's click.
    fn choose_command(&mut self, command: CommandId, cx: &mut Context<Self>) {
        self.open_index = None;
        self.open_submenu.clear();
        self.highlight = None;
        // The command (host slot) decides where focus goes; don't restore the
        // opener's focus over it. Just drop the saved handle.
        self.restore_focus = None;
        cx.emit(MenuCommand(command));
        cx.notify();
    }
}

/// Strip a `&` mnemonic from a top-level title (the menu tree carries `&File`
/// etc. for the palette's mnemonic handling; the bar shows the bare word).
fn clean_title(label: &str) -> String {
    label.replace('&', "")
}

/// The lowercase Alt-mnemonic character of a `&`-marked title (`"&File"` ⇒ `'f'`),
/// or `None` if the label has no `&`. Drives the Alt+letter menu accelerators.
fn mnemonic_of(label: &str) -> Option<char> {
    let pos = label.find('&')?;
    label[pos + 1..].chars().next().map(|c| c.to_ascii_lowercase())
}

/// Apply the C++ `applyMenuBarTitleCase` transform to a `&`-stripped title
/// (main.cpp:1063-1093). `title_case == true` ⇒ the whole word upper-cased;
/// `false` ⇒ Title-Cased — the first letter of every word capitalized, the rest
/// lower-cased, with the "capitalize next" flag re-armed after any whitespace
/// (exactly the C++ char-walk). Pure; unit-tested.
fn cased_title(clean: &str, title_case: bool) -> String {
    if title_case {
        return clean.to_uppercase();
    }
    let mut result = String::with_capacity(clean.len());
    let mut capitalize_next = true;
    for ch in clean.chars() {
        if ch.is_alphabetic() {
            if capitalize_next {
                result.extend(ch.to_uppercase());
            } else {
                result.extend(ch.to_lowercase());
            }
            capitalize_next = false;
        } else {
            result.push(ch);
            if ch.is_whitespace() {
                capitalize_next = true;
            }
        }
    }
    result
}

impl Render for MenuBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // While a top-level menu is open, hold focus in its deferred dropdown so the
        // keyboard can dismiss it (Escape, bound on the dropdown). Save the opener's
        // focus the first time we grab it so the cancel paths can hand it back.
        if self.open_index.is_some() && !self.dropdown_focus.contains_focused(window, cx) {
            if self.restore_focus.is_none() {
                self.restore_focus = window.focused(cx).map(|h| h.downgrade());
            }
            window.focus(&self.dropdown_focus, cx);
        }
        // One top-level menu per submenu, in declaration order
        // (File · Edit · View · Tools · Plugins · Help).
        let menus = self.menus.clone();
        let open_index = self.open_index;
        // The open cascading fly-out chain within the active top-level menu.
        let open_submenu = self.open_submenu.clone();
        // Snapshot the checked-command set once here (where `self` is borrowed) so
        // the dropdown builders don't re-`read` this same entity mid-render.
        let checked = self.checked.clone();
        let title_case = self.title_case;
        let dropdown_focus = self.dropdown_focus.clone();
        let highlight = self.highlight;

        gpui_component::h_flex()
            .id("rcx-menubar")
            .flex_none()
            .items_center()
            .gap(px(tokens::space::XXS))
            .children(menus.into_iter().enumerate().filter_map(move |(i, node)| {
                match node {
                    MenuNode::Submenu { label, children } => Some(render_top_level(
                        i,
                        cased_title(&clean_title(&label), title_case),
                        children,
                        open_index == Some(i),
                        open_submenu.clone(),
                        checked.clone(),
                        dropdown_focus.clone(),
                        // Highlight only applies to the menu that is actually open.
                        if open_index == Some(i) { highlight } else { None },
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
#[allow(clippy::too_many_arguments)]
fn render_top_level(
    index: usize,
    title: String,
    children: Vec<MenuNode>,
    open: bool,
    open_submenu: Vec<usize>,
    checked: HashSet<CommandId>,
    dropdown_focus: FocusHandle,
    highlight: Option<usize>,
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
                            // Hold focus so Escape reaches the dropdown; the parent
                            // `MenuBar::render` focuses this handle while open.
                            .track_focus(&dropdown_focus)
                            // Keyboard navigation of the open menu: ↑/↓ move the
                            // highlight, Enter activates, Tab / Shift+Tab walk the
                            // top-level menus (File · Edit · …), ←/→ traverse
                            // submenus, Escape dismisses (handing focus back to the
                            // editor). The C++ QMenuBar keyboard walk. Capture-phase
                            // so it intercepts Tab / arrows BEFORE gpui's focus
                            // traversal or a window-level binding can swallow them
                            // (bubble-phase on_key_down only ever saw Escape).
                            .capture_key_down(cx.listener(
                                |this, ev: &KeyDownEvent, window, cx| {
                                    let shift = ev.keystroke.modifiers.shift;
                                    match ev.keystroke.key.as_str() {
                                        "escape" => {
                                            cx.stop_propagation();
                                            this.close_menus_restoring(window, cx);
                                        }
                                        "down" => {
                                            cx.stop_propagation();
                                            this.move_highlight(1, cx);
                                        }
                                        "up" => {
                                            cx.stop_propagation();
                                            this.move_highlight(-1, cx);
                                        }
                                        "enter" | "space" => {
                                            cx.stop_propagation();
                                            this.activate_highlight(cx);
                                        }
                                        "right" => {
                                            cx.stop_propagation();
                                            this.right_key(cx);
                                        }
                                        "left" => {
                                            cx.stop_propagation();
                                            this.back_or_prev_menu(cx);
                                        }
                                        "tab" => {
                                            cx.stop_propagation();
                                            this.switch_menu(if shift { -1 } else { 1 }, cx);
                                        }
                                        _ => {}
                                    }
                                },
                            ))
                            .occlude()
                            .top(px(tokens::space::XS))
                            // Dismiss when the user clicks anywhere outside the
                            // dropdown (the C++ menu loses focus → closes).
                            .on_mouse_down_out(cx.listener(|this, _ev, window, cx| {
                                this.close_menus_restoring(window, cx);
                            }))
                            // The whole cascading chain (this menu + any open
                            // fly-outs) is rendered relative to the top-level
                            // dropdown so each child anchors to the right of its
                            // parent row.
                            .child(menu_dropdown(
                                &children,
                                &open_submenu,
                                &checked,
                                highlight,
                                cx,
                            )),
                    ),
            ))
        })
        .into_any_element()
}

/// The dropdown body for a top-level menu's `children` — the root of a cascading
/// chain. `open_submenu` is the chain of child indices whose fly-outs are open
/// (within this top-level menu). Delegates to [`menu_panel`] at depth 0.
fn menu_dropdown(
    children: &[MenuNode],
    open_submenu: &[usize],
    checked: &HashSet<CommandId>,
    highlight: Option<usize>,
    cx: &mut Context<MenuBar>,
) -> impl IntoElement {
    menu_panel(children, &[], open_submenu, checked, highlight, cx)
}

/// Render one cascading menu **panel**: an elevated surface of inset rows for
/// `children`. `parent_path` is the absolute index chain from the top-level
/// dropdown down to (but not including) this panel's rows; `open_path` is the
/// open fly-out chain *relative to this panel* (its head, if any, is the child
/// index whose fly-out is open here, the tail recurses).
///
/// Leaves are clickable command rows; separators are 1px rules; submenus render
/// a "▸" fly-out trigger row that, while open, anchors its child panel to the
/// right of the row (the C++ cascading `QMenu`). Hovering a "▸" row opens its
/// fly-out (replacing any sibling fly-out); hovering a plain leaf collapses any
/// deeper fly-out at this level (the classic menu slide behaviour).
fn menu_panel(
    children: &[MenuNode],
    parent_path: &[usize],
    open_path: &[usize],
    checked: &HashSet<CommandId>,
    highlight: Option<usize>,
    cx: &mut Context<MenuBar>,
) -> AnyElement {
    // The host's checked-command set (the C++ checkable `QAction` state — e.g.
    // `view.scanner` while the scanner pop-out is open) is threaded down from
    // `render` so every row's checkmark stays in lockstep with the live app state
    // without re-`read`ing this entity mid-render.
    let is_checked = |command: &str| checked.contains(command);
    // Depth of this panel = number of fly-outs already crossed to reach it.
    let depth = parent_path.len();
    let open_here = open_path.first().copied();
    // The keyboard highlight applies to the DEEPEST open panel only (the one with
    // no further fly-out open below it).
    let is_deepest = open_path.is_empty();
    let row_highlighted = |i: usize| is_deepest && highlight == Some(i);

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
                    row_highlighted(i),
                    depth,
                    cx,
                )
                .into_any_element(),
            ),
            MenuNode::Submenu {
                label,
                children: sub,
            } => {
                // Absolute path to this submenu's fly-out panel.
                let mut abs_path = parent_path.to_vec();
                abs_path.push(i);
                let is_open = open_here == Some(i);
                // Recurse into the child panel only while this fly-out is open.
                let child_panel: Option<AnyElement> = if is_open {
                    Some(menu_panel(
                        sub,
                        &abs_path,
                        &open_path[1..],
                        checked,
                        highlight,
                        cx,
                    ))
                } else {
                    None
                };
                rows.push(
                    submenu_row(i, label, is_open, row_highlighted(i), abs_path, child_panel, cx)
                        .into_any_element(),
                );
            }
        }
    }

    elevated_surface(cx)
        .occlude()
        .min_w(px(220.0))
        .p(px(tokens::space::XS))
        .child(gpui_component::v_flex().gap(px(1.0)).children(rows))
        .into_any_element()
}

/// A cascading-submenu trigger row: `[gap] Label ……………………… ▸`, opening its child
/// panel to the right while `open`. Hovering the row opens its fly-out (via
/// [`MenuBar::open_submenu_path`] with `abs_path`); the child panel is rendered
/// as a `deferred(...)` absolutely positioned at the row's right edge so it
/// floats over siblings (the C++ cascading `QMenu`). The row highlights while
/// open.
fn submenu_row(
    key: usize,
    label: &str,
    open: bool,
    highlighted: bool,
    abs_path: Vec<usize>,
    child_panel: Option<AnyElement>,
    cx: &mut Context<MenuBar>,
) -> impl IntoElement {
    let fg = color::text(cx);
    let muted = color::text_muted(cx);

    // Empty leading slot mirrors the command row's checkmark slot so submenu
    // labels line up with sibling leaves.
    let lead_slot = div()
        .flex_none()
        .w(px(tokens::font::UI_MD))
        .h(px(tokens::font::UI_MD));

    let hover_path = abs_path.clone();
    div()
        .id(("menu-submenu", key))
        .relative()
        .child(
            gpui_component::h_flex()
                .w_full()
                .h(px(26.0))
                .px(px(tokens::space::SM))
                .gap(px(tokens::space::SM))
                .items_center()
                .justify_between()
                .rounded(px(tokens::radius::MD))
                .text_size(px(tokens::font::UI_MD))
                .text_color(fg)
                .cursor_pointer()
                // Keyboard selection wins (clear selection bg); else the open
                // fly-out's parent row keeps the faint hover tint.
                .when(highlighted, |r| r.bg(color::selected_bg(cx)))
                .when(open && !highlighted, |r| r.bg(color::hover_overlay(cx)))
                .when(!open && !highlighted, |r| {
                    r.hover(|s| s.bg(color::hover_overlay(cx)))
                })
                .child(lead_slot)
                .child(div().flex_1().min_w_0().child(label.to_string()))
                // The fly-out affordance (the C++ submenu ▸).
                .child(
                    div()
                        .flex_none()
                        .text_color(muted)
                        .child(icon::chevron_right().xsmall()),
                ),
        )
        // Open this fly-out when the pointer rests on the row (replacing any
        // sibling fly-out at this level).
        .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
            if *hovered {
                this.open_submenu_path(hover_path.clone(), cx);
            }
        }))
        // The child panel floats to the right of this row, in a higher paint
        // layer (`deferred` + priority) so it sits over sibling rows. Positioned
        // at the row's right edge (`left_full`) and level with its top within the
        // row's `relative()` box — the C++ cascading `QMenu` placement.
        .when_some(child_panel, |this, panel| {
            this.child(
                deferred(
                    div()
                        .occlude()
                        .absolute()
                        .left_full()
                        .top_0()
                        // Nudge the fly-out off the parent panel's edge.
                        .ml(px(tokens::space::XS))
                        .child(panel),
                )
                .priority(1),
            )
        })
}

/// A clickable command row: `[checkmark slot] Label …………… [Shortcut]`.
/// Hover overlay, `MD` radius, disabled rows greyed + inert (the C++ menu shows
/// disabled actions). When `checked`, the leading slot shows a real SVG check
/// (the Assets-stage [`icon::check`]) — the C++ checkable `QAction` (e.g. View ▸
/// Memory Scanner while the scanner pop-out is open). Clicking emits
/// [`MenuCommand`] (via [`MenuBar::choose_command`]) and closes the menu.
#[allow(clippy::too_many_arguments)]
fn command_row(
    key: usize,
    label: &str,
    shortcut: &str,
    command: CommandId,
    enabled: bool,
    checked: bool,
    highlighted: bool,
    depth: usize,
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
        // The keyboard-highlighted row carries the clear selection background (the
        // same one the command palette / type selector use), not the faint hover
        // tint — a persistent keyboard selection needs to read at a glance.
        .when(highlighted, |r| r.bg(color::selected_bg(cx)))
        // Resting the pointer on a plain leaf at this level collapses any open
        // fly-out from a *sibling* "▸" row (the C++ slide-off-the-submenu close).
        .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
            if *hovered {
                this.collapse_submenu_to(depth, cx);
            }
        }))
        .when(enabled, |r| {
            r.cursor_pointer()
                .hover(|s| s.bg(color::hover_overlay(cx)))
                // Dispatch on mouse-DOWN, not click (mouse-up). A fly-out submenu
                // panel (Import/Export/Examples/Data Source) is a `deferred` child
                // positioned OUTSIDE the top-level dropdown's bounds, so pressing a
                // fly-out item triggers the dropdown's `on_mouse_down_out` (a
                // down-outside-me close) which tears the row down BEFORE its
                // mouse-up/click could fire — leaving every submenu item a no-op.
                // Acting on the down event runs the command in the same dispatch
                // pass, before the close takes effect next frame.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev: &MouseDownEvent, _window, cx| {
                        cx.stop_propagation();
                        this.choose_command(command.clone(), cx);
                    }),
                )
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

#[cfg(test)]
mod tests {
    // Only the gpui-free helpers are unit-tested headlessly (importing `super::*`
    // would pull the module's `gpui::*` glob into the test hygiene expansion; see
    // the titlebar.rs note).
    use super::{cased_title, clean_title};

    #[test]
    fn clean_title_strips_mnemonic_ampersand() {
        assert_eq!(clean_title("&File"), "File");
        assert_eq!(clean_title("&Edit"), "Edit");
        assert_eq!(clean_title("Help"), "Help");
        // Only `&` is stripped; the rest is untouched.
        assert_eq!(clean_title("&Save As…"), "Save As…");
    }

    #[test]
    fn cased_title_uppercases_when_title_case_on() {
        // titleCase == true → the whole word upper-cased (the C++ `toUpper()`).
        assert_eq!(cased_title("File", true), "FILE");
        assert_eq!(cased_title("Save As", true), "SAVE AS");
    }

    #[test]
    fn cased_title_title_cases_when_off() {
        // titleCase == false → first letter of every word capitalized, the rest
        // lower-cased, re-armed after whitespace (the C++ char-walk).
        assert_eq!(cased_title("FILE", false), "File");
        assert_eq!(cased_title("save as", false), "Save As");
        assert_eq!(cased_title("hELP", false), "Help");
        // Non-letters pass through; a space re-arms the capitalize flag.
        assert_eq!(cased_title("new  class", false), "New  Class");
    }
}
