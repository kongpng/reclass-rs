//! The window's message / prompt dialog views — the unsaved-changes, generic
//! confirm, text-prompt, and type-aliases dialogs — extracted from the oversized
//! window.rs into a sibling module. A flat sibling (`crate::ui::dialogs::window_dialogs`)
//! so the dialogs' `super::{design,messagebox,dialogs}` references still resolve
//! to the ui submodules.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::ActiveTheme;

/// The three-way unsaved-changes guard dialog (the C++ `closeEvent`'s
/// `ThemedMessageBox::unsavedChanges`): Cancel / Discard / Save changes, with the
/// dirty document names listed and **Save changes** as the default (Enter)
/// button. Built from [`messagebox::unsaved_changes`] (button order + labels +
/// variants + default) so the layout matches the rest of the themed message
/// boxes; emits a [`messagebox::UnsavedChoice`] mapped via
/// [`messagebox::unsaved_choice_for`]. Replaces the old 2-button confirm that
/// quit/closed WITHOUT ever offering Save (item 1).
pub(crate) struct RcxUnsavedDialog {
    spec: crate::ui::dialogs::messagebox::MessageSpec,
    focus_handle: FocusHandle,
}

impl RcxUnsavedDialog {
    pub(crate) fn new(title: &str, text: &str, dirty_names: Vec<String>, cx: &mut Context<Self>) -> Self {
        Self {
            spec: crate::ui::dialogs::messagebox::unsaved_changes(title, text, dirty_names),
            focus_handle: cx.focus_handle(),
        }
    }

    /// Emit the choice for `button_index` (0=Cancel, 1=Discard, 2=Save), per the
    /// `[Cancel, Discard, Save changes]` order [`messagebox::unsaved_changes`]
    /// builds.
    fn choose(&mut self, button_index: usize, cx: &mut Context<Self>) {
        cx.emit(crate::ui::dialogs::messagebox::unsaved_choice_for(button_index));
    }
}

impl Focusable for RcxUnsavedDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<crate::ui::dialogs::messagebox::UnsavedChoice> for RcxUnsavedDialog {}

impl Render for RcxUnsavedDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::ui::dialogs::modal;
        use crate::ui::dialogs::messagebox::{ButtonVariant, DetailLayout};
        use gpui_component::button::{Button, ButtonVariants as _};

        let card_w = modal::clamp_width(crate::ui::dialogs::messagebox::MSG_MAX_WIDTH, window);
        let detail_layout = crate::ui::dialogs::messagebox::format_detail(&self.spec.detail);

        // Body: the count sentence, then the dirty-name detail as a single
        // word-wrapped muted label (the C++ setDetailText shows one QLabel — no
        // list, no threshold). Left-inset to align under the body text.
        let mut body = modal::body(cx).child(
            div()
                .text_size(px(crate::ui::design::tokens::font::UI_MD))
                .text_color(crate::ui::design::color::text(cx))
                .child(self.spec.text.clone()),
        );
        body = match detail_layout {
            DetailLayout::None => body,
            DetailLayout::Label(s) => body.child(
                div()
                    .pl(px(crate::ui::design::tokens::space::MD))
                    .text_size(px(crate::ui::design::tokens::font::UI_SM))
                    .text_color(crate::ui::design::color::text_muted(cx))
                    .child(s),
            ),
        };

        // Footer: the buttons in the spec's left→right order [Cancel, Discard,
        // Save changes], each carrying its variant.
        let mut footer = modal::footer(cx);
        for (i, b) in self.spec.buttons.iter().enumerate() {
            let label = b.label.clone();
            let btn = Button::new(("unsaved-btn", i))
                .label(label)
                .map(|btn| match b.variant {
                    ButtonVariant::Primary => btn.primary(),
                    ButtonVariant::Secondary => btn,
                    ButtonVariant::Destructive => btn.danger(),
                })
                .on_click(cx.listener(move |this, _e, _w, cx| this.choose(i, cx)));
            footer = footer.child(btn);
        }

        modal::card(cx)
            .id("rcx-unsaved-dialog")
            .track_focus(&self.focus_handle)
            .key_context("RcxUnsaved")
            // Enter → the default (Save changes = last button); Esc → Cancel
            // (index 0) — the C++ default-button / reject wiring.
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
                match ev.keystroke.key.as_str() {
                    "enter" => {
                        let last = this.spec.buttons.len().saturating_sub(1);
                        this.choose(last, cx);
                        cx.stop_propagation();
                    }
                    "escape" => {
                        this.choose(0, cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .w(card_w)
            .child(modal::header(self.spec.title.clone(), cx))
            .child(body)
            .child(footer)
    }
}

/// The outcome of an [`RcxConfirmDialog`].
#[derive(Clone, Copy, Debug)]
pub(crate) enum ConfirmChoice {
    Accept,
    Cancel,
}

/// A hand-built two-button confirm dialog used in place of the gpui-component
/// `AlertDialog` when the **default button must be honoured** — chiefly
/// destructive confirms, where the C++ parks initial focus on Cancel so a stray
/// Enter can't destroy work (`themed_messagebox.cpp:181-203`). The AlertDialog
/// hard-binds Enter→OK with no per-button focus hook, so we render the footer
/// ourselves and map Enter to the spec's `default` button (Cancel for
/// destructive) and Esc to Cancel. The destructive action is reachable only by an
/// explicit click on its button.
pub(crate) struct RcxConfirmDialog {
    spec: crate::ui::dialogs::messagebox::MessageSpec,
    focus_handle: FocusHandle,
}

impl RcxConfirmDialog {
    pub(crate) fn new(spec: crate::ui::dialogs::messagebox::MessageSpec, cx: &mut Context<Self>) -> Self {
        Self {
            spec,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Index of the accept button — the last one (confirm specs build
    /// `[Cancel, Accept]`).
    fn accept_index(&self) -> usize {
        self.spec.buttons.len().saturating_sub(1)
    }

    /// Emit Accept iff `button_index` is the accept button, else Cancel.
    fn emit_for(&mut self, button_index: usize, cx: &mut Context<Self>) {
        let choice = if self.spec.buttons.len() > 1 && button_index == self.accept_index() {
            ConfirmChoice::Accept
        } else {
            ConfirmChoice::Cancel
        };
        cx.emit(choice);
    }
}

impl Focusable for RcxConfirmDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<ConfirmChoice> for RcxConfirmDialog {}

impl Render for RcxConfirmDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::ui::dialogs::modal;
        use crate::ui::dialogs::messagebox::{ButtonVariant, DefaultButton, DetailLayout};
        use gpui_component::button::{Button, ButtonVariants as _};

        let card_w = modal::clamp_width(crate::ui::dialogs::messagebox::MSG_MAX_WIDTH, window);

        let mut body = modal::body(cx).child(
            div()
                .text_size(px(crate::ui::design::tokens::font::UI_MD))
                .text_color(crate::ui::design::color::text(cx))
                .child(self.spec.text.clone()),
        );
        if let DetailLayout::Label(s) = crate::ui::dialogs::messagebox::format_detail(&self.spec.detail) {
            body = body.child(
                div()
                    .pl(px(crate::ui::design::tokens::space::MD))
                    .text_size(px(crate::ui::design::tokens::font::UI_SM))
                    .text_color(crate::ui::design::color::text_muted(cx))
                    .child(s),
            );
        }

        // Footer: spec order [Cancel, Accept], each with its variant (the accept of
        // a destructive confirm is `.danger()` / red).
        let mut footer = modal::footer(cx);
        for (i, b) in self.spec.buttons.iter().enumerate() {
            let btn = Button::new(("confirm-btn", i))
                .label(b.label.clone())
                .map(|btn| match b.variant {
                    ButtonVariant::Primary => btn.primary(),
                    ButtonVariant::Secondary => btn,
                    ButtonVariant::Destructive => btn.danger(),
                })
                .on_click(cx.listener(move |this, _e, _w, cx| this.emit_for(i, cx)));
            footer = footer.child(btn);
        }

        // Enter → the spec's DEFAULT button (Cancel for destructive confirms, so a
        // stray Enter can't destroy work); Esc → Cancel (index 0). The destructive
        // action fires only on an explicit click of its button.
        let enter_index = match self.spec.default {
            DefaultButton::Accept => self.accept_index(),
            DefaultButton::Cancel => 0,
        };

        modal::card(cx)
            .id("rcx-confirm-dialog")
            .track_focus(&self.focus_handle)
            .key_context("RcxConfirm")
            .capture_key_down(cx.listener(move |this, ev: &KeyDownEvent, _w, cx| {
                match ev.keystroke.key.as_str() {
                    "enter" => {
                        this.emit_for(enter_index, cx);
                        cx.stop_propagation();
                    }
                    "escape" => {
                        this.emit_for(0, cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .w(card_w)
            .child(modal::header(self.spec.title.clone(), cx))
            .child(body)
            .child(footer)
    }
}

/// The outcome of the [`TextPromptDialog`] (the C++ `QInputDialog` accept/reject).
#[derive(Clone, Debug)]
pub(crate) enum TextPromptEvent {
    /// OK / Enter on a non-empty value — the trimmed text.
    Accept(String),
    /// Cancel / Esc.
    Cancel,
}

/// A minimal modal free-text input dialog — the port's `QInputDialog::getText`.
/// A single input seeded with a default (all-selected), an OK button (disabled
/// while the trimmed text is empty), and Cancel. Enter confirms; Esc cancels.
pub(crate) struct TextPromptDialog {
    title: String,
    label: String,
    input: Entity<gpui_component::input::InputState>,
    focus_handle: FocusHandle,
    _sub: Subscription,
}

impl TextPromptDialog {
    pub(crate) fn new(
        title: String,
        label: String,
        default: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        use gpui_component::input::{InputEvent, InputState};
        let input = cx.new(|cx| {
            let mut s = InputState::new(window, cx);
            s.set_value(default.clone(), window, cx);
            s
        });
        // Re-render on change so the OK enabled-state tracks the field.
        let sub = cx.subscribe_in(&input, window, |_this, _i, ev: &InputEvent, _w, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });
        TextPromptDialog {
            title,
            label,
            input,
            focus_handle: cx.focus_handle(),
            _sub: sub,
        }
    }

    fn value(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let text = self.value(cx).trim().to_string();
        if !text.is_empty() {
            cx.emit(TextPromptEvent::Accept(text));
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(TextPromptEvent::Cancel);
    }
}

impl Focusable for TextPromptDialog {
    /// Delegate the dialog focus to the INPUT so `window.focus(focus_handle)`
    /// lands keystrokes in the field (the command-palette pattern) — without this
    /// the dialog card holds focus and typing does nothing (the same dead-input
    /// failure mode the cross-cutting note flags for inline edits).
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl EventEmitter<TextPromptEvent> for TextPromptDialog {}

impl Render for TextPromptDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::ui::dialogs::modal;
        use gpui_component::button::{Button, ButtonVariants as _};
        use gpui_component::input::Input;
        use gpui_component::Disableable as _;

        let can_ok = !self.value(cx).trim().is_empty();
        let card_w = modal::clamp_width(420., window);

        let body = modal::body(cx)
            .child(modal::field_label(self.label.clone(), cx))
            .child(Input::new(&self.input).w_full());

        let footer = modal::footer(cx)
            .child(
                Button::new("prompt-cancel")
                    .label("Cancel")
                    .on_click(cx.listener(|this, _e, _w, cx| this.cancel(cx))),
            )
            .child(
                Button::new("prompt-ok")
                    .primary()
                    .label("OK")
                    .when(!can_ok, |b| b.disabled(true))
                    .on_click(cx.listener(|this, _e, _w, cx| this.confirm(cx))),
            );

        modal::card(cx)
            .id("rcx-text-prompt")
            .track_focus(&self.focus_handle)
            .key_context("RcxTextPrompt")
            // Capture-phase Enter/Esc so the dialog confirms/cancels even while the
            // input owns focus (the C++ dialog's default-button / reject wiring).
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
                match ev.keystroke.key.as_str() {
                    "enter" => {
                        this.confirm(cx);
                        cx.stop_propagation();
                    }
                    "escape" => {
                        this.cancel(cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .w(card_w)
            .child(
                modal::header_with_close(self.title.clone(), "prompt-close", cx.listener(|this, _e, _w, cx| this.cancel(cx)), cx),
            )
            .child(body)
            .child(footer)
    }
}

/// The outcome of the [`TypeAliasesDialog`].
#[derive(Clone, Debug)]
pub(crate) enum TypeAliasesEvent {
    /// OK — commit the edited alias map.
    Accept,
    /// Cancel / Esc.
    Cancel,
}

/// The Tools ▸ Type Aliases editor (the C++ `showTypeAliasesDialog`): a row per
/// aliasable [`NodeKind`] (the primitives + pointers + strings; the C++ skips
/// Vec/Mat/Struct/Array), each with the canonical type name on the left and an
/// editable alias field on the right. Two preset buttons fill the column with the
/// stdint (C99) or Windows (basetsd.h) names; Clear empties them.
pub(crate) struct TypeAliasesDialog {
    /// (kind, canonical-name, alias-input) per aliasable kind, in `K_KIND_META`
    /// order.
    rows: Vec<(
        crate::core::NodeKind,
        &'static str,
        Entity<gpui_component::input::InputState>,
    )>,
    focus_handle: FocusHandle,
}

impl TypeAliasesDialog {
    fn aliasable(kind: crate::core::NodeKind) -> bool {
        use crate::core::NodeKind::*;
        !matches!(kind, Vec2 | Vec3 | Vec4 | Mat4x4 | Struct | Array)
    }

    /// The Windows (basetsd.h) preset alias for a kind, if any (the C++
    /// `kWindowsPreset`).
    fn windows_alias(kind: crate::core::NodeKind) -> Option<&'static str> {
        use crate::core::NodeKind::*;
        Some(match kind {
            Int8 => "CHAR",
            Int16 => "SHORT",
            Int32 => "LONG",
            Int64 => "LONGLONG",
            UInt8 => "UCHAR",
            UInt16 => "USHORT",
            UInt32 => "ULONG",
            UInt64 => "ULONGLONG",
            Float => "FLOAT",
            Double => "DOUBLE",
            Bool => "BOOLEAN",
            Pointer32 => "ULONG",
            Pointer64 => "ULONG_PTR",
            _ => return None,
        })
    }

    pub(crate) fn new(
        current: std::collections::HashMap<crate::core::NodeKind, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        use gpui_component::input::InputState;
        let mut rows = Vec::new();
        for meta in crate::core::K_KIND_META.iter() {
            if !Self::aliasable(meta.kind) {
                continue;
            }
            let seed = current.get(&meta.kind).cloned().unwrap_or_default();
            let input = cx.new(|cx| {
                let mut s = InputState::new(window, cx).placeholder(meta.type_name);
                if !seed.is_empty() {
                    s.set_value(seed, window, cx);
                }
                s
            });
            rows.push((meta.kind, meta.type_name, input));
        }
        TypeAliasesDialog {
            rows,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Collect the edited alias map (empty fields are dropped — no alias).
    pub(crate) fn collect(&self, cx: &App) -> std::collections::HashMap<crate::core::NodeKind, String> {
        let mut map = std::collections::HashMap::new();
        for (kind, _name, input) in &self.rows {
            let v = input.read(cx).value().to_string();
            let v = v.trim().to_string();
            if !v.is_empty() {
                map.insert(*kind, v);
            }
        }
        map
    }

    /// Fill every field with the stdint (C99) preset — the canonical type name
    /// per kind (the C++ `kStdintPreset`).
    fn apply_stdint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (_kind, name, input) in &self.rows {
            let name = name.to_string();
            input.update(cx, |s, cx| s.set_value(name, window, cx));
        }
        cx.notify();
    }

    /// Fill the Windows-mapped fields with their basetsd.h names; clear the rest.
    fn apply_windows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (kind, _name, input) in &self.rows {
            let v = Self::windows_alias(*kind).unwrap_or("").to_string();
            input.update(cx, |s, cx| s.set_value(v, window, cx));
        }
        cx.notify();
    }

    /// Empty every field (no aliases).
    fn apply_clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (_kind, _name, input) in &self.rows {
            input.update(cx, |s, cx| s.set_value(String::new(), window, cx));
        }
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        cx.emit(TypeAliasesEvent::Accept);
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(TypeAliasesEvent::Cancel);
    }
}

impl Focusable for TypeAliasesDialog {
    /// Focus the first alias input so the dialog opens ready to type (the
    /// command-palette delegate pattern); fall back to the card handle if there
    /// are no rows.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.rows
            .first()
            .map(|(_, _, input)| input.read(cx).focus_handle(cx))
            .unwrap_or_else(|| self.focus_handle.clone())
    }
}

impl EventEmitter<TypeAliasesEvent> for TypeAliasesDialog {}

impl Render for TypeAliasesDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::ui::design::tokens;
        use crate::ui::dialogs::modal;
        use gpui_component::button::{Button, ButtonVariants as _};
        use gpui_component::input::Input;
        use gpui_component::Sizable as _;

        let card_w = modal::clamp_width(460., window);
        let card_max_h = modal::clamp_height(560., 100., window);
        let mono = SharedString::from(tokens::font::mono_family());

        let rows: Vec<AnyElement> = self
            .rows
            .iter()
            .map(|(_kind, name, input)| {
                gpui_component::h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(tokens::space::MD))
                    .child(
                        div()
                            .w(px(110.))
                            .flex_none()
                            .font_family(mono.clone())
                            .text_size(px(tokens::font::UI_SM))
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from(*name)),
                    )
                    // The input fills the REMAINING width after the fixed label via
                    // a `flex_1` wrapper (the palette/enum/source pattern). Using
                    // `Input::new(..).w_full()` DIRECTLY as a flex child sized it to
                    // 100% of the row, so it overran the 110px label + gap and the
                    // card's `overflow_hidden` clipped its right edge (the cut-off
                    // right side). `min_w_0` lets it shrink within the row.
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(input).w_full().font_family(mono.clone())),
                    )
                    .into_any_element()
            })
            .collect();

        let presets = gpui_component::h_flex()
            .gap(px(tokens::space::SM))
            .child(
                Button::new("alias-stdint")
                    .small()
                    .label("stdint (C99)")
                    .on_click(cx.listener(|this, _e, window, cx| this.apply_stdint(window, cx))),
            )
            .child(
                Button::new("alias-windows")
                    .small()
                    .label("Windows (basetsd.h)")
                    .on_click(cx.listener(|this, _e, window, cx| this.apply_windows(window, cx))),
            )
            .child(
                Button::new("alias-clear")
                    .small()
                    .label("Clear")
                    .on_click(cx.listener(|this, _e, window, cx| this.apply_clear(window, cx))),
            );

        let body = modal::body(cx).child(presets).child(
            gpui_component::v_flex()
                .id("rcx-alias-rows")
                .w_full()
                .max_h(px(360.))
                .overflow_y_scroll()
                .gap(px(tokens::space::XS))
                .children(rows),
        );

        let footer = modal::footer(cx)
            .child(
                Button::new("alias-cancel")
                    .label("Cancel")
                    .on_click(cx.listener(|this, _e, _w, cx| this.cancel(cx))),
            )
            .child(
                Button::new("alias-ok")
                    .primary()
                    .label("OK")
                    .on_click(cx.listener(|this, _e, _w, cx| this.confirm(cx))),
            );

        modal::card(cx)
            .id("rcx-type-aliases")
            .track_focus(&self.focus_handle)
            .key_context("RcxTypeAliases")
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
                if ev.keystroke.key.as_str() == "escape" {
                    this.cancel(cx);
                    cx.stop_propagation();
                }
            }))
            .w(card_w)
            .max_h(card_max_h)
            .child(modal::header_with_close("Type Aliases", "alias-close", cx.listener(|this, _e, _w, cx| this.cancel(cx)), cx))
            .child(body)
            .child(footer)
    }
}
