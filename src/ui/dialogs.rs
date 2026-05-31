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
//! - [`optionsdialog`](super::optionsdialog) — Options dialog (§5).
//! - [`gotoaddress`](super::gotoaddress) — Goto-Address dialog (§7).
//! - [`commandpalette`](super::commandpalette) — Command palette (§8).
//! - [`findbar`](super::findbar) — the Find bar (Ctrl+F).
//! - [`messagebox`](super::messagebox) — themed message/confirm/input (§3,§4).
//! - [`typeselectorpopup`](super::typeselectorpopup) — type picker (§9).
//! - [`enumpicker`](super::enumpicker) — enum picker (§10).
//! - [`sourcechooser`](super::sourcechooser) — source chooser (§11).
//! - [`hextoolbar`](super::hextoolbar) — hex toolbar (§12).
//! - [`contextmenu`](super::contextmenu) — context menus (§0).
//! - [`tooltip`](super::tooltip) — tooltips + hover previews (§15,§20).
//!
//! This module keeps the [`DialogKind`] catalogue (a stable, logged identifier for
//! each surface, used for action routing + tests) and re-exports the views.
//!
//! Gated behind the `ui` feature (the dialogs render via gpui-component).

// Re-export the implemented dialog/popup views + their event types under one
// roof (the "open a dialog" entry point the window wires to).
#[cfg(feature = "ui")]
pub use super::commandpalette::{CommandPalette, PaletteEvent};
#[cfg(feature = "ui")]
pub use super::enumpicker::{EnumPickerEvent, EnumPickerPopup};
#[cfg(feature = "ui")]
pub use super::findbar::{FindBar, FindEvent};
#[cfg(feature = "ui")]
pub use super::gotoaddress::{GotoAddressDialog, GotoEvent};
#[cfg(feature = "ui")]
pub use super::hextoolbar::{HexToolbarEvent, HexToolbarPopup};
#[cfg(feature = "ui")]
pub use super::optionsdialog::{OptionsDialog, OptionsEvent};
#[cfg(feature = "ui")]
pub use super::sourcechooser::{SourceChooserEvent, SourceChooserPopup};
#[cfg(feature = "ui")]
pub use super::typeselectorpopup::{TypeSelectorEvent, TypeSelectorPopup};

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
