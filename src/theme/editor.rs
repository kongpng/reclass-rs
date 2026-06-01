//! `ThemeEditor` — port of `src/themes/themeeditor.{h,cpp}`.
//!
//! A modal that edits a [`Theme`] field-by-field with live preview. The Qt
//! widgets dialog becomes a gpui view rendered through the window's modal
//! `Dialog` layer. Split, like the other dialogs, into:
//!
//! - [`ThemeEditorState`] — the gpui-free reducer: the working theme + selected
//!   index, `load_theme`, `set_field`, `set_name`, `file_info`. Unit-tested
//!   headlessly against the `themeeditor.cpp` logic.
//! - [`ThemeEditor`] / [`ThemeEditorEvent`] (behind `ui`) — the gpui view: the
//!   theme-selector combo, the editable Name field, the file-info line, the
//!   grouped scrollable swatch grid (each row a clickable swatch + hex opening
//!   a color picker), Save (persists the user JSON) and Cancel (reverts the
//!   live preview).
//!
//! The transient-broadcast semantics (preview-on-open, preview-on-edit,
//! revert-on-cancel) live in [`crate::theme::ThemeManager`] and are driven by
//! the view; the live re-style itself is applied through
//! [`crate::ui::theme_apply::apply_theme`].

use crate::theme::manager::ThemeManager;
use crate::theme::model::{FieldId, Theme};

/// Whether a theme slot is a pristine built-in (edits become a user copy) or a
/// real file (a user theme, or a built-in already overridden on disk). Mirrors
/// the C++ `themeFilePath(idx).isEmpty()` branch — except the Rust
/// [`ThemeManager::theme_file_path`] always returns a path for in-range slots,
/// so we distinguish via [`ThemeManager::builtin_count`] + the modified diff.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FileInfo {
    /// A pristine built-in: "Built-in theme (edits save as user copy)".
    BuiltinCopy,
    /// A backing file path: "File: <path>".
    File(String),
}

impl FileInfo {
    /// The label shown under the Name field (`m_fileInfoLabel`,
    /// `themeeditor.cpp:69-72`).
    pub fn label(&self) -> String {
        match self {
            FileInfo::BuiltinCopy => "Built-in theme (edits save as user copy)".to_string(),
            FileInfo::File(p) => format!("File: {p}"),
        }
    }
}

/// The gpui-free editor reducer — port of the `ThemeEditor` member state
/// (`themeeditor.h:22-24`) and its `loadTheme` / name / field mutators.
#[derive(Clone, Debug)]
pub struct ThemeEditorState {
    /// The working copy being edited (`m_theme`).
    theme: Theme,
    /// The index it was loaded from (`m_themeIndex`).
    index: usize,
}

impl ThemeEditorState {
    /// `ThemeEditor(themeIndex)` (`themeeditor.cpp:25-30`): seed from `all[index]`
    /// if in range, else `current`.
    pub fn new(mgr: &ThemeManager, index: usize) -> Self {
        let all = mgr.themes();
        let theme = all
            .get(index)
            .cloned()
            .unwrap_or_else(|| mgr.current().clone());
        ThemeEditorState { theme, index }
    }

    /// `loadTheme(index)` (`themeeditor.cpp:154-172`): switch the working copy to
    /// `all[index]`; out-of-range is a no-op. Returns `true` if the index was in
    /// range (so the caller previews + refreshes the swatches).
    pub fn load_theme(&mut self, mgr: &ThemeManager, index: usize) -> bool {
        let all = mgr.themes();
        if index >= all.len() {
            return false;
        }
        self.index = index;
        self.theme = all[index].clone();
        true
    }

    /// `m_nameEdit textChanged` (`themeeditor.cpp:58-60`): update the working
    /// name in place (does not touch the manager).
    pub fn set_name(&mut self, name: impl Into<String>) {
        self.theme.name = name.into();
    }

    /// `pickColor` apply step (`themeeditor.cpp:191-193`): set one field of the
    /// working copy. The caller then previews.
    pub fn set_field(&mut self, id: FieldId, color: crate::theme::Color) {
        self.theme.set(id, Some(color));
    }

    /// Read a field's current color (for the swatch). `None` when the field is
    /// unset (renders as `#000000`, matching `QColor().name()`).
    pub fn field(&self, id: FieldId) -> Option<crate::theme::Color> {
        self.theme.get(id)
    }

    /// `result()` — the working theme to commit on Save (`themeeditor.h:19`).
    pub fn result(&self) -> Theme {
        self.theme.clone()
    }

    /// `selectedIndex()` — the slot being edited (`themeeditor.h:20`).
    pub fn selected_index(&self) -> usize {
        self.index
    }

    /// The working name (the Name field text).
    pub fn name(&self) -> &str {
        &self.theme.name
    }

    /// The file-info classification for the current slot
    /// (`themeeditor.cpp:69-72` / `164-166`). A pristine built-in →
    /// [`FileInfo::BuiltinCopy`]; anything with a backing file → its path.
    pub fn file_info(&self, mgr: &ThemeManager) -> FileInfo {
        // A pristine built-in (unmodified on disk) shows the "edits save as user
        // copy" hint; an overridden built-in or a user theme shows its path.
        let is_pristine_builtin = self.index < mgr.builtin_count() && {
            // theme_file_path points into builtin_dir only when the slot equals
            // its pristine default (the manager's own modified-diff). We mirror
            // that test by comparing the path's parent against the user dir.
            match mgr.theme_file_path(self.index) {
                Some(p) => !p.starts_with(mgr_user_dir(mgr)),
                None => true,
            }
        };
        if is_pristine_builtin {
            FileInfo::BuiltinCopy
        } else {
            match mgr.theme_file_path(self.index) {
                Some(p) => FileInfo::File(p.display().to_string()),
                None => FileInfo::BuiltinCopy,
            }
        }
    }
}

/// The user-themes directory (the manager doesn't expose it directly; we infer
/// it from a known user-index path, falling back to the default dir).
fn mgr_user_dir(mgr: &ThemeManager) -> std::path::PathBuf {
    // Any user-index path lives under the user dir; if there are none, use the
    // default user dir. A built-in's *override* path also lives there.
    let n = mgr.themes().len();
    if n > mgr.builtin_count() {
        if let Some(p) = mgr.theme_file_path(mgr.builtin_count()) {
            if let Some(parent) = p.parent() {
                return parent.to_path_buf();
            }
        }
    }
    ThemeManager::default_user_dir()
}

// ── gpui view ────────────────────────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use view::{ThemeEditor, ThemeEditorEvent};

#[cfg(feature = "ui")]
mod view {
    use super::ThemeEditorState;
    use crate::theme::model::{FieldId, THEME_FIELDS};
    use crate::theme::Color;
    use crate::ui::design::{color as ui_color, tokens};
    use crate::ui::dialogs::modal;
    use crate::ui::theme_apply::{apply_theme, to_hsla, ThemeRegistryGlobal};
    use gpui::*;
    use gpui_component::button::{Button, ButtonVariants as _};
    use gpui_component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
    use gpui_component::input::{Input, InputEvent, InputState};
    use gpui_component::popover::Popover;
    use gpui_component::{Colorize as _, Sizable as _};

    /// The dialog's outcome, raised to the host (the C++ `accept`/`reject`).
    #[derive(Clone, Debug)]
    pub enum ThemeEditorEvent {
        /// Save pressed: the index that was committed (the host syncs its theme
        /// menu / state; the editor has already persisted via the manager).
        Saved(usize),
        /// Cancel / Esc: the live preview has been reverted.
        Cancelled,
    }

    /// One swatch's binding: its field id, label, and dedicated color-picker.
    struct Swatch {
        field: FieldId,
        label: &'static str,
        picker: Entity<ColorPickerState>,
    }

    /// The Theme Editor modal — port of `class ThemeEditor`
    /// (`themeeditor.{h,cpp}`).
    pub struct ThemeEditor {
        /// The working reducer (theme + index + file-info).
        state: ThemeEditorState,
        /// The Name field.
        name_input: Entity<InputState>,
        /// One color picker per theme field (the clickable swatch grid). Ordered
        /// to match [`THEME_FIELDS`].
        swatches: Vec<Swatch>,
        /// Whether the theme-selector combo popover is open.
        combo_open: bool,
        /// The theme being previewed when the editor opened (revert target).
        original: crate::theme::Theme,
        /// Set while we programmatically write `name_input` / swatches so the
        /// resulting `Change` events don't recurse / clobber the working copy.
        suppress_name_change: bool,
        suppress_swatch_change: bool,
        focus_handle: FocusHandle,
        _subscriptions: Vec<Subscription>,
    }

    impl ThemeEditor {
        /// Build the editor for `theme_index` and start the live preview
        /// (`themeeditor.cpp:25-150`).
        pub fn new(theme_index: usize, window: &mut Window, cx: &mut Context<Self>) -> Self {
            let mgr_rc = ThemeRegistryGlobal::get(cx);
            let (state, original) = {
                let mgr = mgr_rc.borrow();
                (
                    ThemeEditorState::new(&mgr, theme_index),
                    mgr.current().clone(),
                )
            };

            let name = state.name().to_string();
            let name_input = cx.new(|cx| InputState::new(window, cx).default_value(name));

            let mut subs = Vec::new();
            // Name edits update the working theme + preview live.
            subs.push(cx.subscribe_in(
                &name_input,
                window,
                |this, input, ev: &InputEvent, window, cx| {
                    if let InputEvent::Change = ev {
                        if this.suppress_name_change {
                            this.suppress_name_change = false;
                            return;
                        }
                        let text = input.read(cx).value().to_string();
                        this.state.set_name(text);
                        this.preview(window, cx);
                    }
                },
            ));

            // One picker per theme field, seeded with the field's current color.
            // Each picker's Change routes to its own field + previews live.
            let mut swatches = Vec::with_capacity(THEME_FIELDS.len());
            for f in THEME_FIELDS {
                let current = state.field(f.id).unwrap_or(Color::rgb(0, 0, 0));
                let picker =
                    cx.new(|cx| ColorPickerState::new(window, cx).default_value(to_hsla(current)));
                let field = f.id;
                subs.push(cx.subscribe_in(
                    &picker,
                    window,
                    move |this, _picker, ev: &ColorPickerEvent, window, cx| {
                        if this.suppress_swatch_change {
                            return;
                        }
                        let ColorPickerEvent::Change(value) = ev;
                        if let Some(hsla) = value {
                            if let Some(c) = hsla_to_color(*hsla) {
                                this.state.set_field(field, c);
                                this.preview(window, cx);
                            }
                        }
                    },
                ));
                swatches.push(Swatch {
                    field: f.id,
                    label: f.label,
                    picker,
                });
            }

            // Start the live preview of the working copy (the C++ ctor tail
            // `tm.previewTheme(m_theme)`).
            {
                let mut mgr = mgr_rc.borrow_mut();
                mgr.preview_theme(state.result());
            }
            apply_theme(&state.result(), window, cx);

            ThemeEditor {
                state,
                name_input,
                swatches,
                combo_open: false,
                original,
                suppress_name_change: false,
                suppress_swatch_change: false,
                focus_handle: cx.focus_handle(),
                _subscriptions: subs,
            }
        }

        /// Read-only access to the reducer (for tests / wiring).
        pub fn state(&self) -> &ThemeEditorState {
            &self.state
        }

        /// Broadcast the working theme as a transient preview + re-style the live
        /// window (`previewTheme` + the `themeChanged` re-style; the Rust port
        /// applies the theme directly so the preview shows even without a wired
        /// observer).
        fn preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
            let working = self.state.result();
            let mgr_rc = ThemeRegistryGlobal::get(cx);
            mgr_rc.borrow_mut().preview_theme(working.clone());
            apply_theme(&working, window, cx);
            cx.notify();
        }

        /// `loadTheme(index)` (`themeeditor.cpp:154-172`): switch the edited slot,
        /// resync the Name field + swatches, and preview.
        fn load_theme(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
            let mgr_rc = ThemeRegistryGlobal::get(cx);
            let ok = mgr_rc.borrow().themes().len() > index
                && self.state.load_theme(&mgr_rc.borrow(), index);
            if !ok {
                return;
            }
            self.combo_open = false;
            // Resync the name field without recursing into the change handler.
            self.suppress_name_change = true;
            let name = self.state.name().to_string();
            self.name_input
                .update(cx, |s, cx| s.set_value(name, window, cx));
            // Resync every swatch picker to the newly-loaded theme's colors,
            // suppressing the resulting Change events (they'd otherwise feed the
            // old working copy back in field-by-field).
            self.suppress_swatch_change = true;
            for sw in &self.swatches {
                let c = self.state.field(sw.field).unwrap_or(Color::rgb(0, 0, 0));
                sw.picker
                    .update(cx, |p, cx| p.set_value(to_hsla(c), window, cx));
            }
            self.suppress_swatch_change = false;
            self.preview(window, cx);
        }

        /// Save: commit the working theme into the manager (which persists the
        /// user JSON + commits the preview), re-style, and emit `Saved`
        /// (`themeeditor.cpp:138` → `QDialog::accept` → the host's
        /// `updateTheme`).
        fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
            let working = self.state.result();
            let index = self.state.selected_index();
            let mgr_rc = ThemeRegistryGlobal::get(cx);
            let committed = {
                let mut mgr = mgr_rc.borrow_mut();
                mgr.update_theme(index, working); // persists + commits preview + emits
                mgr.current().clone()
            };
            apply_theme(&committed, window, cx);
            cx.emit(ThemeEditorEvent::Saved(index));
        }

        /// Cancel: revert the live preview to the pre-open theme + emit
        /// (`themeeditor.cpp:139-142`).
        fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
            let mgr_rc = ThemeRegistryGlobal::get(cx);
            mgr_rc.borrow_mut().revert_preview();
            // Re-apply the original so the live window snaps back even without a
            // wired observer.
            apply_theme(&self.original, window, cx);
            cx.emit(ThemeEditorEvent::Cancelled);
        }

        // ── Rendering ──

        /// The theme-selector combo (`m_themeCombo`, `themeeditor.cpp:39-51`).
        fn render_theme_combo(&self, cx: &mut Context<Self>) -> impl IntoElement {
            let mgr_rc = ThemeRegistryGlobal::get(cx);
            let names: Vec<String> = mgr_rc
                .borrow()
                .themes()
                .iter()
                .map(|t| t.name.clone())
                .collect();
            let current = self.state.selected_index();
            let label = names
                .get(current)
                .cloned()
                .unwrap_or_else(|| format!("Theme #{current}"));
            let editor = cx.entity().downgrade();

            gpui_component::h_flex()
                .w_full()
                .items_center()
                .gap(px(tokens::space::MD))
                .child(modal::field_label("Theme:", cx))
                .child(
                    Popover::new("theme-edit-combo")
                        .anchor(Anchor::TopLeft)
                        .open(self.combo_open)
                        .on_open_change(cx.listener(|this, open: &bool, _window, cx| {
                            this.combo_open = *open;
                            cx.notify();
                        }))
                        .trigger(
                            Button::new("theme-edit-combo-trig")
                                .outline()
                                .small()
                                .label(SharedString::from(format!("{label}  \u{2304}"))),
                        )
                        .content(move |_state, _window, cx| {
                            let mut menu = gpui_component::v_flex()
                                .min_w(px(180.))
                                .p(px(tokens::space::XS))
                                .gap(px(1.))
                                .bg(ui_color::elevated_bg(cx))
                                .border_1()
                                .border_color(ui_color::border(cx))
                                .rounded(px(tokens::radius::LG));
                            for (i, name) in names.iter().enumerate() {
                                let editor = editor.clone();
                                menu = menu.child(
                                    crate::ui::design::zed_list_row(
                                        SharedString::from(format!("theme-edit-row-{i}")),
                                        i == current,
                                        cx,
                                    )
                                    .text_size(px(tokens::font::UI_SM))
                                    .cursor_pointer()
                                    .on_click(move |_e, window, cx| {
                                        editor
                                            .update(cx, |this, cx| this.load_theme(i, window, cx))
                                            .ok();
                                    })
                                    .child(name.clone()),
                                );
                            }
                            menu
                        }),
                )
        }

        /// One swatch row (`themeeditor.cpp:97-126`): label + the field's color
        /// picker (a clickable swatch that pops the HSLA / palette picker) + the
        /// hex string.
        fn render_swatch_row(&self, sw: &Swatch, cx: &mut Context<Self>) -> impl IntoElement {
            let current = self.state.field(sw.field).unwrap_or(Color::rgb(0, 0, 0));
            let hex = current.to_hex();
            let mono = SharedString::from(tokens::font::mono_family());

            gpui_component::h_flex()
                .w_full()
                .h(px(24.))
                .pl(px(tokens::space::MD))
                .items_center()
                .gap(px(tokens::space::MD))
                .child(
                    div()
                        .w(px(120.))
                        .flex_none()
                        .text_size(px(tokens::font::UI_SM))
                        .text_color(ui_color::text(cx))
                        .child(sw.label),
                )
                // The picker element IS the clickable swatch (it renders the
                // current color as its trigger button and pops the full picker).
                .child(ColorPicker::new(&sw.picker).small())
                .child(
                    div()
                        .font_family(mono)
                        .text_size(px(tokens::font::UI_XS))
                        .text_color(ui_color::text_muted(cx))
                        .child(hex),
                )
        }

        /// The grouped, scrollable swatch grid (`themeeditor.cpp:84-131`): a
        /// section header on each group change, then the field rows.
        fn render_swatches(&self, cx: &mut Context<Self>) -> impl IntoElement {
            let mut col = gpui_component::v_flex().w_full().gap(px(2.));
            let mut current_group: Option<&'static str> = None;
            // THEME_FIELDS and self.swatches are 1:1 in order.
            for (f, sw) in THEME_FIELDS.iter().zip(self.swatches.iter()) {
                if current_group != Some(f.group) {
                    col = col.child(crate::ui::design::section_label(f.group, cx));
                    current_group = Some(f.group);
                }
                col = col.child(self.render_swatch_row(sw, cx));
            }
            col
        }
    }

    /// Convert a gpui `Hsla` (from the picker) back to our 8-bit [`Color`] via
    /// its hex string (the picker's own `to_hex` → our `Color::parse`).
    fn hsla_to_color(hsla: Hsla) -> Option<Color> {
        // gpui_component's Colorize::to_hex yields "#rrggbb" or "#rrggbbaa";
        // take the leading 6 hex digits (we drop alpha — the theme model is RGB).
        let hex = hsla.to_hex();
        if hex.len() >= 7 {
            Color::parse(&hex[..7])
        } else {
            None
        }
    }

    impl Focusable for ThemeEditor {
        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.focus_handle.clone()
        }
    }

    impl EventEmitter<ThemeEditorEvent> for ThemeEditor {}

    impl Render for ThemeEditor {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let card_w = modal::clamp_width(460., window);
            let card_h = modal::clamp_height(640., 60., window);

            let mgr_rc = ThemeRegistryGlobal::get(cx);
            let file_label = self.state.file_info(&mgr_rc.borrow()).label();

            let body = modal::body(cx)
                .child(self.render_theme_combo(cx))
                .child(
                    gpui_component::h_flex()
                        .w_full()
                        .items_center()
                        .gap(px(tokens::space::MD))
                        .child(modal::field_label("Name:", cx))
                        .child(Input::new(&self.name_input).w_full()),
                )
                .child(
                    div()
                        .text_size(px(tokens::font::UI_XS))
                        .text_color(ui_color::text_muted(cx))
                        .child(file_label),
                )
                .child(
                    div()
                        .id("theme-edit-swatches")
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .overflow_y_scroll()
                        .child(self.render_swatches(cx)),
                );

            let footer = modal::footer(cx)
                .child(
                    Button::new("theme-edit-cancel")
                        .label("Cancel")
                        .on_click(cx.listener(|this, _e, window, cx| this.cancel(window, cx))),
                )
                .child(
                    Button::new("theme-edit-save")
                        .primary()
                        .label("Save theme")
                        .on_click(cx.listener(|this, _e, window, cx| this.save(window, cx))),
                );

            modal::card(cx)
                .id("rcx-theme-editor")
                .track_focus(&self.focus_handle)
                .key_context("RcxThemeEditor")
                .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                    if ev.keystroke.key.as_str() == "escape" {
                        this.cancel(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .w(card_w)
                .h(card_h)
                .child(modal::header("Theme Editor", cx).child(modal::close_button(
                    "theme-edit-close",
                    cx.listener(|this, _e, window, cx| this.cancel(window, cx)),
                    cx,
                )))
                .child(body)
                .child(footer)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::color::Color;
    use crate::theme::manager::{MemSettings, ThemeManager};
    use std::path::PathBuf;

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("_oracle/fixtures/themes")
    }

    fn mk_manager() -> (ThemeManager, PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static C: AtomicU64 = AtomicU64::new(0);
        let user_dir = std::env::temp_dir().join(format!(
            "rcx-theme-editor-test-{}-{}",
            std::process::id(),
            C.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::create_dir_all(&user_dir);
        let m = ThemeManager::new(
            Box::new(MemSettings::new()),
            fixtures_dir(),
            user_dir.clone(),
        );
        (m, user_dir)
    }

    #[test]
    fn new_seeds_from_index() {
        let (m, _u) = mk_manager();
        let st = ThemeEditorState::new(&m, 0);
        assert_eq!(st.selected_index(), 0);
        assert_eq!(st.name(), m.themes()[0].name);
    }

    #[test]
    fn new_out_of_range_seeds_from_current() {
        let (m, _u) = mk_manager();
        let st = ThemeEditorState::new(&m, 9999);
        // Falls back to the manager's current theme.
        assert_eq!(st.name(), m.current().name);
    }

    #[test]
    fn set_name_updates_working_copy() {
        let (m, _u) = mk_manager();
        let mut st = ThemeEditorState::new(&m, 0);
        st.set_name("My Theme");
        assert_eq!(st.name(), "My Theme");
        assert_eq!(st.result().name, "My Theme");
    }

    #[test]
    fn set_field_updates_working_copy() {
        let (m, _u) = mk_manager();
        let mut st = ThemeEditorState::new(&m, 0);
        st.set_field(FieldId::Background, Color::rgb(0x12, 0x34, 0x56));
        assert_eq!(
            st.field(FieldId::Background),
            Some(Color::rgb(0x12, 0x34, 0x56))
        );
        assert_eq!(st.result().background, Some(Color::rgb(0x12, 0x34, 0x56)));
    }

    #[test]
    fn load_theme_switches_slot() {
        let (m, _u) = mk_manager();
        let mut st = ThemeEditorState::new(&m, 0);
        let target = if m.themes().len() > 1 { 1 } else { 0 };
        assert!(st.load_theme(&m, target));
        assert_eq!(st.selected_index(), target);
        assert_eq!(st.name(), m.themes()[target].name);
    }

    #[test]
    fn load_theme_out_of_range_is_noop() {
        let (m, _u) = mk_manager();
        let mut st = ThemeEditorState::new(&m, 0);
        assert!(!st.load_theme(&m, 9999));
        assert_eq!(st.selected_index(), 0);
    }

    #[test]
    fn file_info_pristine_builtin_is_copy_hint() {
        let (m, _u) = mk_manager();
        // builtin[0] is pristine on a fresh temp user dir → "edits save as user copy".
        let st = ThemeEditorState::new(&m, 0);
        assert_eq!(st.file_info(&m), FileInfo::BuiltinCopy);
        assert_eq!(
            st.file_info(&m).label(),
            "Built-in theme (edits save as user copy)"
        );
    }

    #[test]
    fn file_info_user_theme_shows_path() {
        let (mut m, user_dir) = mk_manager();
        // Add a user theme; its file-info shows its path under the user dir.
        let mut t = m.themes()[0].clone();
        t.name = "Custom Editor Theme".to_string();
        m.add_theme(t);
        let idx = m.themes().len() - 1;
        let st = ThemeEditorState::new(&m, idx);
        match st.file_info(&m) {
            FileInfo::File(p) => {
                assert!(p.contains("custom_editor_theme.json"), "{p}");
                assert!(p.starts_with(&user_dir.display().to_string()), "{p}");
            }
            other => panic!("expected File, got {other:?}"),
        }
    }

    #[test]
    fn file_info_label_formats() {
        assert_eq!(
            FileInfo::BuiltinCopy.label(),
            "Built-in theme (edits save as user copy)"
        );
        assert_eq!(
            FileInfo::File("/tmp/x.json".to_string()).label(),
            "File: /tmp/x.json"
        );
    }
}
