//! Dialogs & popups — the modal/overlay seam (stub).
//!
//! Port target: the C++ dialogs/popups (widgets-dialogs.md) — Options dialog,
//! Goto-Address dialog, themed message/confirm boxes, the command palette, and
//! the type/enum/source picker popups. Per ARCHITECTURE §5 these map onto
//! gpui-component `Dialog` / `AlertDialog` / `Popover` / `List` / `Select`,
//! rendered through the [`Root`](gpui_component::Root) overlay layers the window
//! already mounts (`render_dialog_layer` / `render_sheet_layer` /
//! `render_notification_layer`).
//!
//! **STUB** — no dialog is implemented in the foundation stage; this module only
//! names the seam (a [`DialogKind`] enum cataloguing the surfaces) so later
//! workflows have a home and a stable reference. The real views are added by the
//! dialogs/command-palette UI workflows.
//!
//! Gated behind the `ui` feature (the dialogs render via gpui-component).

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
    /// Type selector popup (widgets-dialogs §9) → `Popover` + `List`/`Select`.
    TypeSelector,
    /// Enum picker popup (widgets-dialogs §10) → `Popover` + `List`.
    EnumPicker,
    /// Source chooser popup (widgets-dialogs §11) → `Popover` + `List`.
    SourceChooser,
}

impl DialogKind {
    /// Short identifier (for logging / future action routing).
    pub fn name(self) -> &'static str {
        match self {
            DialogKind::Options => "options",
            DialogKind::GotoAddress => "goto-address",
            DialogKind::MessageBox => "message-box",
            DialogKind::CommandPalette => "command-palette",
            DialogKind::TypeSelector => "type-selector",
            DialogKind::EnumPicker => "enum-picker",
            DialogKind::SourceChooser => "source-chooser",
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
