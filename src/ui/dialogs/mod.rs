//! Dialogs & popups — the modal/overlay catalogue + the implemented surfaces.
//!
//! Port target: the C++ dialogs/popups (widgets-dialogs.md) — Options dialog,
//! Goto-Address dialog, themed message/confirm boxes, the command palette, the
//! find bar, the type/enum/source picker popups, the hex toolbar, context menus,
//! and tooltips. Per ARCHITECTURE §5 these map onto gpui-component `Dialog` /
//! `AlertDialog` / `Popover` / `List` / `Select`, rendered through the
//! [`Root`](gpui_component::Root) overlay layers the window mounts
//! (`render_dialog_layer` / `render_sheet_layer` / `render_notification_layer`).
//!
//! Each surface lives in its own sibling module (gpui-free model + a thin view):
//! - [`optionsdialog`](crate::ui::dialogs::optionsdialog) — Options dialog (§5).
//! - [`gotoaddress`](crate::ui::dialogs::gotoaddress) — Goto-Address dialog (§7).
//! - [`commandpalette`](crate::ui::pickers::commandpalette) — Command palette (§8).
//! - [`findbar`](crate::ui::overlays::findbar) — the Find bar (Ctrl+F).
//! - [`messagebox`](crate::ui::dialogs::messagebox) — themed message/confirm/input (§3,§4).
//! - [`typeselectorpopup`](crate::ui::pickers::typeselectorpopup) — type picker (§9).
//! - [`enumpicker`](crate::ui::pickers::enumpicker) — enum picker (§10).
//! - [`sourcechooser`](crate::ui::pickers::sourcechooser) — source chooser (§11).
//! - [`hextoolbar`](crate::ui::overlays::hextoolbar) — hex toolbar (§12).
//! - [`contextmenu`](crate::ui::overlays::contextmenu) — context menus (§0).
//! - [`tooltip`](crate::ui::overlays::tooltip) — tooltips + hover previews (§15,§20).
//!
//! This module keeps the [`DialogKind`] catalogue (a stable, logged identifier for
//! each surface, used for action routing + tests) and re-exports the views.
//!
//! Gated behind the `ui` feature (the dialogs render via gpui-component).

pub mod messagebox;
pub mod plugin_manager;
pub mod window_dialogs;
pub mod optionsdialog;
pub mod gotoaddress;
pub mod profilerdialog;

// Re-export the implemented dialog/popup views + their event types under one
// roof (the "open a dialog" entry point the window wires to).
#[cfg(feature = "ui")]
pub use crate::ui::pickers::commandpalette::{CommandPalette, PaletteEvent};
#[cfg(feature = "ui")]
pub use crate::ui::pickers::enumpicker::{EnumPickerEvent, EnumPickerPopup};
#[cfg(feature = "ui")]
pub use crate::ui::overlays::findbar::{FindBar, FindEvent};
#[cfg(feature = "ui")]
pub use crate::ui::dialogs::gotoaddress::{GotoAddressDialog, GotoEvent};
#[cfg(feature = "ui")]
pub use crate::ui::overlays::hextoolbar::{HexToolbarEvent, HexToolbarPopup};
#[cfg(feature = "ui")]
pub use crate::ui::dialogs::optionsdialog::{OptionsDialog, OptionsEvent, OptionsPage, OptionsResult};
#[cfg(feature = "ui")]
pub use crate::ui::dialogs::profilerdialog::{ProfilerDialog, ProfilerEvent};
#[cfg(feature = "ui")]
pub use crate::ui::pickers::sourcechooser::{SourceChooserEvent, SourceChooserPopup};
#[cfg(feature = "ui")]
pub use crate::ui::pickers::typeselectorpopup::{TypeSelectorEvent, TypeSelectorPopup};

/// The modal/overlay surfaces to be ported (widgets-dialogs.md). A catalogue
/// only — each variant becomes its own gpui-component-backed view later.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DialogKind {
    /// Options dialog: nav tree + stacked pages + search (widgets-dialogs §5)
    /// → `Dialog` + `Tree` + `TextInput`.
    Options,
    /// Goto-Address dialog: live-validated address input (widgets-dialogs §7)
    /// → `Dialog` + `TextInput`.
    GotoAddress,
    /// Themed message / confirm box (widgets-dialogs §3,4) → `AlertDialog`.
    MessageBox,
    /// Command palette: filtered list + input (widgets-dialogs §8)
    /// → `List` with `perform_search` in a centered modal.
    CommandPalette,
    /// Find bar (Ctrl+F) → an inline `TextInput` + nav buttons over the editor.
    FindBar,
    /// Type selector popup (widgets-dialogs §9) → `Popover` + `List`/`Select`.
    TypeSelector,
    /// Enum picker popup (widgets-dialogs §10) → `Popover` + `List`.
    EnumPicker,
    /// Source chooser popup (widgets-dialogs §11) → `Popover` + `List`.
    SourceChooser,
    /// Hex toolbar popup (widgets-dialogs §12) → `Popover` + custom canvas.
    HexToolbar,
}

impl DialogKind {
    /// Short identifier (for logging / future action routing).
    pub fn name(self) -> &'static str {
        match self {
            DialogKind::Options => "options",
            DialogKind::GotoAddress => "goto-address",
            DialogKind::MessageBox => "message-box",
            DialogKind::CommandPalette => "command-palette",
            DialogKind::FindBar => "find-bar",
            DialogKind::TypeSelector => "type-selector",
            DialogKind::EnumPicker => "enum-picker",
            DialogKind::SourceChooser => "source-chooser",
            DialogKind::HexToolbar => "hex-toolbar",
        }
    }
}

// ── Shared Zed modal scaffolding (the reusable frame the dialogs build on) ────

/// The reusable Zed-modal frame helpers — a centered elevated card with a header
/// (title + optional close), a comfortable body, and a right-aligned footer
/// button row. Every owned modal (`optionsdialog`, `gotoaddress`,
/// `processpicker`, …) renders its body inside [`modal::frame`] so the chrome
/// stays identical (and themeable via the shared [`design`](crate::ui::design)
/// tokens).
///
/// Gated behind the `ui` feature (it pulls gpui / gpui-component).
#[cfg(feature = "ui")]
pub mod modal {
    use crate::ui::design::{color, tokens};
    use gpui::{
        div, px, App, ClickEvent, Div, FontWeight, InteractiveElement as _, IntoElement,
        ParentElement as _, Pixels, SharedString, StatefulInteractiveElement as _, Styled, Window,
    };

    /// The standard modal padding (`XL` = 16px) applied to header/body/footer.
    pub const PAD: f32 = tokens::space::XL;

    /// The minimum horizontal breathing room kept on **each** side of a modal card
    /// so it never butts against (or overflows) the window edge. The gpui-component
    /// dialog layer centers a card by `x = window_center - card_width/2` with **no**
    /// lower clamp, so a card wider than the window pushes its right edge (and its
    /// footer buttons) off-screen. Reserving this margin keeps the whole card —
    /// buttons included — inside the visible window even when it is small.
    pub const EDGE_MARGIN: f32 = tokens::space::XXL;

    /// Clamp a *desired* modal width to what the current window can actually show.
    ///
    /// Returns `min(desired, viewport_width - 2·EDGE_MARGIN)` (never below a sane
    /// floor). This is the in-view counterpart of "modal dialogs clamp to the
    /// window bounds, not the full screen": pass the design width you'd like and
    /// you get a width that always fits the live window, so the card stays fully
    /// visible (and its buttons reachable) even when the window is narrower than
    /// the design size. Use it for the card's `.w(...)` instead of a hard `px`.
    pub fn clamp_width(desired: f32, window: &Window) -> Pixels {
        // A floor so the card is still usable on a pathologically tiny window.
        const FLOOR: f32 = 280.0;
        let avail = f32::from(window.viewport_size().width) - 2.0 * EDGE_MARGIN;
        px(desired.min(avail).max(FLOOR.min(desired)))
    }

    /// Clamp a *desired* modal height to the current window (the vertical analogue
    /// of [`clamp_width`]). The dialog layer offsets a card down by `margin_top`
    /// and never clamps the bottom, so a tall fixed-height card can run off the
    /// bottom on a short window; capping the height keeps the footer on-screen.
    /// `reserved_top` is the space already spent above the card (its `margin_top`).
    pub fn clamp_height(desired: f32, reserved_top: f32, window: &Window) -> Pixels {
        const FLOOR: f32 = 200.0;
        let avail = f32::from(window.viewport_size().height) - reserved_top - EDGE_MARGIN;
        px(desired.min(avail).max(FLOOR.min(desired)))
    }

    /// The elevated modal card shell: `elevated_bg`, 1px border, `XL` (8px)
    /// radius, and a soft drop shadow (`.shadow_lg()`). Callers set their own
    /// width/height and add the header/body/footer children. This is the
    /// executable counterpart of zed_ui_spec §5.8.
    pub fn card(cx: &App) -> Div {
        div()
            .flex()
            .flex_col()
            .bg(color::elevated_bg(cx))
            .border_1()
            .border_color(color::border(cx))
            .rounded(px(tokens::radius::XL))
            .shadow_lg()
            .text_color(color::text(cx))
            .overflow_hidden()
    }

    /// A modal header strip: the title (`UI_LG` semibold) on the left, with a 1px
    /// bottom border. Callers that want a `×` close affordance add
    /// [`close_button`] as a trailing child (the row already lays out with
    /// `justify_between`).
    pub fn header(title: impl Into<SharedString>, cx: &App) -> Div {
        let title: SharedString = title.into();
        gpui_component::h_flex()
            .w_full()
            .px(px(PAD))
            .py(px(tokens::space::LG))
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(color::border(cx))
            .child(
                div()
                    .flex_none()
                    .text_size(px(tokens::font::UI_LG))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(color::text(cx))
                    .child(title),
            )
    }

    /// The header's `×` close button: a ghost square that lightens on hover.
    /// `id` must be unique within the window (gpui interactivity).
    pub fn close_button(
        id: &'static str,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
        cx: &App,
    ) -> impl IntoElement {
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .size(px(20.))
            .rounded(px(tokens::radius::MD))
            .text_size(px(tokens::font::UI_MD))
            .text_color(color::text_muted(cx))
            .hover(|s| s.bg(color::hover_overlay(cx)))
            .cursor_pointer()
            .on_click(on_click)
            .child("\u{2715}")
    }

    /// A modal header row with the `title` on the left and the `×` close button
    /// (with unique `id`, running `on_close`) on the right — the shared header
    /// every dialog builds.
    pub fn header_with_close(
        title: impl Into<SharedString>,
        id: &'static str,
        on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
        cx: &App,
    ) -> Div {
        header(title, cx).child(close_button(id, on_close, cx))
    }

    /// The modal body container: comfortable `XL` padding, vertical stack with a
    /// `LG` gap, and it grows to fill the card (`flex_1`). Callers add the
    /// labelled rows / sections / controls.
    pub fn body(cx: &App) -> Div {
        let _ = cx;
        gpui_component::v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .p(px(PAD))
            .gap(px(tokens::space::LG))
    }

    /// The footer button row: a 1px top border, `XL` horizontal / `LG` vertical
    /// padding, right-aligned, with an `MD` gap between buttons. Convention
    /// `{secondary…, primary}` left→right (the C++ `makeButtonRow`). Callers add
    /// the gpui-component `Button`s as children.
    pub fn footer(cx: &App) -> Div {
        gpui_component::h_flex()
            .w_full()
            .px(px(PAD))
            .py(px(tokens::space::LG))
            .gap(px(tokens::space::MD))
            .items_center()
            .justify_end()
            .border_t_1()
            .border_color(color::border(cx))
    }

    /// A field label (the small left-of-control caption inside a settings row /
    /// goto input): `UI_SM` muted, medium weight.
    pub fn field_label(text: impl Into<SharedString>, cx: &App) -> Div {
        div()
            .text_size(px(tokens::font::UI_SM))
            .font_weight(FontWeight::MEDIUM)
            .text_color(color::text_muted(cx))
            .child(text.into())
    }

    /// A muted helper / description line (`UI_SM`, muted) under a control.
    pub fn help_text(text: impl Into<SharedString>, cx: &App) -> Div {
        div()
            .text_size(px(tokens::font::UI_SM))
            .text_color(color::text_muted(cx))
            .child(text.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_kind_names_are_stable() {
        assert_eq!(DialogKind::Options.name(), "options");
        assert_eq!(DialogKind::CommandPalette.name(), "command-palette");
        assert_eq!(DialogKind::GotoAddress.name(), "goto-address");
    }
}
