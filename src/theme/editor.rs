//! `ThemeEditor` GPUI view — port of `src/themes/themeeditor.{h,cpp}`.
//!
//! The Qt-Widgets modal becomes a GPUI view rendered through gpui-component's
//! modal `Dialog` (`gpui_component_cookbook.md`). **SKELETON** — the full
//! swatch grid / `Select` / `TextInput` / `ColorPicker` wiring is ported by the
//! `ui` workflow (ARCHITECTURE.md §9). The behavioural contract the rest of the
//! app depends on (preview-on-open, preview-on-edit, revert-on-cancel — the
//! transient-broadcast semantics of [`crate::theme::ThemeManager::preview_theme`]
//! / [`revert_preview`](crate::theme::ThemeManager::revert_preview)) lives in
//! the manager, which is fully implemented and tested.
//!
//! Gated behind the `ui` feature so the headless logic build never pulls gpui.

use crate::theme::model::Theme;

/// `class ThemeEditor` (`themeeditor.h`). Holds the working copy + selected
/// index; `result()` / `selected_index()` expose what the caller commits.
/// **SKELETON.**
pub struct ThemeEditor {
    theme: Theme,
    theme_index: usize,
}

impl ThemeEditor {
    /// `ThemeEditor(index)` (`themeeditor.cpp:31-156`). **SKELETON.**
    pub fn new(theme: Theme, theme_index: usize) -> Self {
        ThemeEditor { theme, theme_index }
    }

    /// `result()` — the working theme to commit on Save.
    pub fn result(&self) -> Theme {
        self.theme.clone()
    }

    /// `selectedIndex()` — the index the editor is editing.
    pub fn selected_index(&self) -> usize {
        self.theme_index
    }
}
