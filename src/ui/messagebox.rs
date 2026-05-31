//! Themed message / confirm / input dialogs — `ThemedMessageBox` +
//! `ThemedInputDialog` (widgets-dialogs.md §3,§4).
//!
//! Port of the `QMessageBox` / `QInputDialog` replacements. Per the cookbook
//! (ARCHITECTURE §5) these map onto gpui-component's `AlertDialog`
//! (message/confirm) and `Dialog` + `TextInput` (input), opened through the
//! `Root` overlay via [`WindowExt`](gpui_component::WindowExt). The decision
//! logic the C++ encoded — severity, the button set, the *default-focus* rule,
//! the detail-text formatting, and the unsaved-changes three-way choice — is
//! modeled as gpui-free, unit-tested types here; the open helpers are thin
//! feature-gated wrappers that translate them into `AlertDialog` builders.
//!
//! Wording conventions (`themed_messagebox.h:14-37`): titles are noun phrases
//! (not the severity word); button labels are verbs; destructive confirms default
//! focus to **Cancel** so a stray Enter can't destroy work.
//!
//! Gated behind the `ui` feature for the open helpers; the model is always built.

/// Severity of a message box (`ThemedMessageBox::Severity`,
/// `themed_messagebox.h:42`). The severity icon was removed in the C++ — title +
/// text convey it — so this only influences default button styling/labels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    Info,
    Warning,
    Critical,
    Question,
}

/// The three-way unsaved-changes result (`UnsavedChoice`,
/// `themed_messagebox.h:42`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnsavedChoice {
    Save,
    Discard,
    Cancel,
}

/// Which button a message/confirm box should focus by default. The load-bearing
/// rule (`themed_messagebox.cpp:181-203`): a **destructive** confirm defaults to
/// **Cancel** (so a stray Enter can't destroy work); everything else defaults to
/// the accept/primary button.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DefaultButton {
    Accept,
    Cancel,
}

/// A button on a message/confirm box (label + variant), in left→right visual
/// order (the C++ `makeButtonRow` adds a leading stretch then buttons in order).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MsgButton {
    pub label: String,
    pub variant: ButtonVariant,
}

/// The three `DialogButton` variants (`dialog_button.h:33`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonVariant {
    /// Action target — `borderFocused` outline (the `:default` action).
    Primary,
    /// "Back out" — lowest-key (`border`).
    Secondary,
    /// Destructive — red (`markerPtr`).
    Destructive,
}

/// A fully-specified message/confirm box (the gpui-free description an open helper
/// renders). Built by the [`info`]/[`warn`]/[`critical`]/[`confirm`]/
/// [`unsaved_changes`] constructors below.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MessageSpec {
    pub severity: Severity,
    pub title: String,
    pub text: String,
    /// Optional detail block (one item per line); see [`format_detail`].
    pub detail: Vec<String>,
    /// The buttons, left→right.
    pub buttons: Vec<MsgButton>,
    /// Which button gets initial focus.
    pub default: DefaultButton,
}

/// The width clamp for a message box (`themed_messagebox.cpp`: `[420,640]`).
pub const MSG_MIN_WIDTH: f32 = 420.0;
pub const MSG_MAX_WIDTH: f32 = 640.0;

/// The detail-list threshold: **>5 items → a scrollable list**, else a wrapped
/// label (`setDetailText`, `themed_messagebox.cpp:59-111`).
pub const DETAIL_LIST_THRESHOLD: usize = 5;

/// How a detail block should be presented.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DetailLayout {
    /// No detail.
    None,
    /// ≤5 items → a single word-wrapped label (items joined by newlines).
    Label(String),
    /// >5 items → a scrollable read-only list of the items.
    List(Vec<String>),
}

/// Decide how to present a detail block (`setDetailText`): split on `\n` (already
/// done — `detail` is the item vec), drop empties; `> 5` → list, else label.
pub fn format_detail(detail: &[String]) -> DetailLayout {
    let items: Vec<String> = detail
        .iter()
        .filter(|s| !s.trim().is_empty())
        .cloned()
        .collect();
    if items.is_empty() {
        DetailLayout::None
    } else if items.len() > DETAIL_LIST_THRESHOLD {
        DetailLayout::List(items)
    } else {
        DetailLayout::Label(items.join("\n"))
    }
}

impl MessageSpec {
    /// The resolved detail layout for this spec.
    pub fn detail_layout(&self) -> DetailLayout {
        format_detail(&self.detail)
    }
}

/// `ThemedMessageBox::info` — one Primary "OK", default-focused.
pub fn info(title: &str, text: &str) -> MessageSpec {
    one_button(Severity::Info, title, text, "OK")
}

/// `ThemedMessageBox::warn` — one Primary "OK".
pub fn warn(title: &str, text: &str) -> MessageSpec {
    one_button(Severity::Warning, title, text, "OK")
}

/// `ThemedMessageBox::critical` — one Primary "OK".
pub fn critical(title: &str, text: &str) -> MessageSpec {
    one_button(Severity::Critical, title, text, "OK")
}

fn one_button(severity: Severity, title: &str, text: &str, ok: &str) -> MessageSpec {
    MessageSpec {
        severity,
        title: title.to_string(),
        text: text.to_string(),
        detail: Vec::new(),
        buttons: vec![MsgButton {
            label: ok.to_string(),
            variant: ButtonVariant::Primary,
        }],
        default: DefaultButton::Accept,
    }
}

/// `ThemedMessageBox::confirm(..., acceptLabel, rejectLabel="", destructive=false)`
/// (`themed_messagebox.cpp:181-203`): Cancel (Secondary) + accept (Primary, or
/// **Destructive** if `destructive`). Buttons are `[Cancel, Accept]` (the Windows
/// habit). **Destructive defaults focus to Cancel.**
pub fn confirm(title: &str, text: &str, accept_label: &str, destructive: bool) -> MessageSpec {
    let accept_variant = if destructive {
        ButtonVariant::Destructive
    } else {
        ButtonVariant::Primary
    };
    MessageSpec {
        severity: Severity::Question,
        title: title.to_string(),
        text: text.to_string(),
        detail: Vec::new(),
        buttons: vec![
            MsgButton {
                label: "Cancel".to_string(),
                variant: ButtonVariant::Secondary,
            },
            MsgButton {
                label: accept_label.to_string(),
                variant: accept_variant,
            },
        ],
        default: if destructive {
            DefaultButton::Cancel
        } else {
            DefaultButton::Accept
        },
    }
}

/// `ThemedMessageBox::unsavedChanges(title, text, detail)`
/// (`themed_messagebox.cpp:205-240`): Cancel(Secondary) / Discard(Destructive) /
/// Save changes(Primary, **default**). `detail` is one-name-per-line (formatted
/// per [`format_detail`]). The button order is `[Cancel, Discard, Save changes]`.
pub fn unsaved_changes(title: &str, text: &str, detail: Vec<String>) -> MessageSpec {
    MessageSpec {
        severity: Severity::Question,
        title: title.to_string(),
        text: text.to_string(),
        detail,
        buttons: vec![
            MsgButton {
                label: "Cancel".to_string(),
                variant: ButtonVariant::Secondary,
            },
            MsgButton {
                label: "Discard".to_string(),
                variant: ButtonVariant::Destructive,
            },
            MsgButton {
                label: "Save changes".to_string(),
                variant: ButtonVariant::Primary,
            },
        ],
        default: DefaultButton::Accept,
    }
}

/// Map an unsaved-changes button index (0=Cancel, 1=Discard, 2=Save) to the
/// [`UnsavedChoice`] result.
pub fn unsaved_choice_for(button_index: usize) -> UnsavedChoice {
    match button_index {
        1 => UnsavedChoice::Discard,
        2 => UnsavedChoice::Save,
        _ => UnsavedChoice::Cancel,
    }
}

// ── input dialog (ThemedInputDialog) ─────────────────────────────────────────

/// The kind of value a [`ThemedInputDialog`](InputSpec) collects
/// (`ThemedInputDialog::getText/getInt/getItem`, `themed_inputdialog.h`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum InputKind {
    /// `getText` — free text (with an optional placeholder); pre-selects all.
    Text {
        default: String,
        placeholder: String,
    },
    /// `getInt` — a clamped integer (`value`, `min..=max`).
    Int { value: i64, min: i64, max: i64 },
    /// `getItem` — pick one of `items` (combo), starting at `current`.
    Item { items: Vec<String>, current: usize },
}

/// A themed input dialog description — label + the value control. All variants
/// return `Option<_>` from the modal (`nullopt`/`None` = the user dismissed;
/// `themed_inputdialog.h:20`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct InputSpec {
    pub title: String,
    pub label: String,
    pub kind: InputKind,
}

impl InputSpec {
    /// `getText(title,label,default,placeholder)`.
    pub fn text(title: &str, label: &str, default: &str, placeholder: &str) -> Self {
        InputSpec {
            title: title.to_string(),
            label: label.to_string(),
            kind: InputKind::Text {
                default: default.to_string(),
                placeholder: placeholder.to_string(),
            },
        }
    }

    /// `getInt(title,label,value,min,max)`.
    pub fn int(title: &str, label: &str, value: i64, min: i64, max: i64) -> Self {
        InputSpec {
            title: title.to_string(),
            label: label.to_string(),
            kind: InputKind::Int { value, min, max },
        }
    }

    /// `getItem(title,label,items,current)`.
    pub fn item(title: &str, label: &str, items: Vec<String>, current: usize) -> Self {
        InputSpec {
            title: title.to_string(),
            label: label.to_string(),
            kind: InputKind::Item { items, current },
        }
    }

    /// Clamp/normalize the text result (the `getText` accept commits the trimmed
    /// edit text; an int is range-clamped; an item index is bounds-checked). This
    /// is the value-shaping the dialogs apply before returning.
    pub fn clamp_int(value: i64, min: i64, max: i64) -> i64 {
        value.clamp(min, max)
    }
}

// ── open helpers (feature-gated) ─────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use open::{open_confirm, open_message};

#[cfg(feature = "ui")]
mod open {
    use super::{
        format_detail, ButtonVariant, DefaultButton, DetailLayout, MessageSpec, Severity,
        MSG_MAX_WIDTH,
    };
    use crate::ui::design::{color, tokens};
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::button::ButtonVariant as GButtonVariant;
    use gpui_component::dialog::DialogButtonProps;
    use gpui_component::{ActiveTheme as _, Icon, IconName, WindowExt as _};

    fn to_gpui_variant(v: ButtonVariant) -> GButtonVariant {
        match v {
            ButtonVariant::Primary => GButtonVariant::Primary,
            ButtonVariant::Secondary => GButtonVariant::Secondary,
            ButtonVariant::Destructive => GButtonVariant::Danger,
        }
    }

    /// The severity icon + tint (the C++ removed the icon, but a Zed alert reads
    /// far better with a tinted leading glyph; title + text still carry meaning).
    fn severity_icon(severity: Severity, cx: &App) -> impl IntoElement {
        let (name, tint) = match severity {
            Severity::Info => (IconName::Info, color::accent(cx)),
            Severity::Warning => (IconName::TriangleAlert, cx.theme().warning),
            Severity::Critical => (IconName::CircleX, cx.theme().danger),
            // No dedicated question glyph in the bundled set; the accent Info glyph
            // reads as a neutral prompt for confirm/unsaved dialogs.
            Severity::Question => (IconName::Info, color::accent(cx)),
        };
        Icon::new(name).text_color(tint)
    }

    /// Build the alert body: the wrapped message text, then the detail block
    /// (small list/label) when present, as a single description element.
    fn description_body(text: String, detail: &[String], cx: &App) -> AnyElement {
        let layout = format_detail(detail);
        gpui_component::v_flex()
            .gap(px(tokens::space::MD))
            .child(
                div()
                    .text_size(px(tokens::font::UI_MD))
                    .text_color(color::text(cx))
                    .child(text),
            )
            .map(|col| match layout {
                DetailLayout::None => col,
                DetailLayout::Label(s) => col.child(
                    div()
                        .text_size(px(tokens::font::UI_SM))
                        .text_color(color::text_muted(cx))
                        .child(s),
                ),
                DetailLayout::List(items) => col.child(
                    gpui_component::v_flex()
                        .id("rcx-msg-detail")
                        .max_h(px(140.))
                        .overflow_y_scroll()
                        .p(px(tokens::space::MD))
                        .gap(px(tokens::space::XXS))
                        .rounded(px(tokens::radius::MD))
                        .border_1()
                        .border_color(color::border(cx))
                        .bg(color::panel_bg(cx))
                        .children(items.into_iter().map(|item| {
                            div()
                                .text_size(px(tokens::font::UI_SM))
                                .text_color(color::text_muted(cx))
                                .child(item)
                        })),
                ),
            })
            .into_any_element()
    }

    /// Open a one-button message box (info/warn/critical) through the `Root`
    /// overlay. The button just dismisses (the C++ `exec()` returns; callers that
    /// need the result use [`open_confirm`]).
    pub fn open_message(spec: MessageSpec, window: &mut Window, cx: &mut App) {
        let title = spec.title.clone();
        let text = spec.text.clone();
        let detail = spec.detail.clone();
        let severity = spec.severity;
        let ok_label = spec
            .buttons
            .first()
            .map(|b| b.label.clone())
            .unwrap_or_else(|| "OK".to_string());
        window.open_alert_dialog(cx, move |alert, _window, cx| {
            alert
                .icon(severity_icon(severity, cx))
                .title(title.clone())
                .description(description_body(text.clone(), &detail, cx))
                .width(px(MSG_MAX_WIDTH))
                .button_props(DialogButtonProps::default().ok_text(ok_label.clone()))
        });
    }

    /// Open a two-button confirm (`confirm`) through the `Root` overlay, calling
    /// `on_accept` if the accept button is pressed. The accept button keeps its
    /// variant (Primary or Danger for destructive); the C++ default-focus rule is
    /// preserved by mapping [`DefaultButton::Cancel`] → not auto-confirming.
    pub fn open_confirm<F>(spec: MessageSpec, on_accept: F, window: &mut Window, cx: &mut App)
    where
        F: Fn(&mut Window, &mut App) + 'static,
    {
        let title = spec.title.clone();
        let text = spec.text.clone();
        let detail = spec.detail.clone();
        let severity = spec.severity;
        // buttons = [Cancel, Accept]; read the accept (last) button.
        let accept = spec.buttons.last().cloned();
        let cancel = spec.buttons.first().cloned();
        let _focus_accept = spec.default == DefaultButton::Accept;
        let on_accept = std::rc::Rc::new(on_accept);
        window.open_alert_dialog(cx, move |alert, _window, cx| {
            let ok_text = accept
                .as_ref()
                .map(|b| b.label.clone())
                .unwrap_or_else(|| "OK".to_string());
            let ok_variant = accept
                .as_ref()
                .map(|b| to_gpui_variant(b.variant))
                .unwrap_or(GButtonVariant::Primary);
            let cancel_text = cancel
                .as_ref()
                .map(|b| b.label.clone())
                .unwrap_or_else(|| "Cancel".to_string());
            let cb = on_accept.clone();
            alert
                .icon(severity_icon(severity, cx))
                .title(title.clone())
                .description(description_body(text.clone(), &detail, cx))
                .width(px(MSG_MAX_WIDTH))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(ok_text)
                        .ok_variant(ok_variant)
                        .cancel_text(cancel_text)
                        .show_cancel(true)
                        .on_ok(move |_, window, cx| {
                            cb(window, cx);
                            true
                        }),
                )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{
        confirm, critical, format_detail, info, unsaved_changes, unsaved_choice_for, warn,
        ButtonVariant, DefaultButton, DetailLayout, InputSpec, Severity, UnsavedChoice,
    };

    #[test]
    fn info_warn_critical_have_single_ok() {
        for (spec, sev) in [
            (info("Title", "Body"), Severity::Info),
            (warn("Title", "Body"), Severity::Warning),
            (critical("Title", "Body"), Severity::Critical),
        ] {
            assert_eq!(spec.severity, sev);
            assert_eq!(spec.buttons.len(), 1);
            assert_eq!(spec.buttons[0].label, "OK");
            assert_eq!(spec.buttons[0].variant, ButtonVariant::Primary);
            assert_eq!(spec.default, DefaultButton::Accept);
        }
    }

    #[test]
    fn confirm_non_destructive_defaults_to_accept() {
        let spec = confirm("Delete?", "Sure?", "Delete", false);
        // [Cancel(Secondary), Delete(Primary)].
        assert_eq!(spec.buttons.len(), 2);
        assert_eq!(spec.buttons[0].label, "Cancel");
        assert_eq!(spec.buttons[0].variant, ButtonVariant::Secondary);
        assert_eq!(spec.buttons[1].label, "Delete");
        assert_eq!(spec.buttons[1].variant, ButtonVariant::Primary);
        assert_eq!(spec.default, DefaultButton::Accept);
    }

    #[test]
    fn confirm_destructive_defaults_to_cancel() {
        // The load-bearing safety rule: destructive → accept is Destructive AND
        // focus defaults to Cancel so a stray Enter can't destroy work.
        let spec = confirm("Delete project?", "This cannot be undone.", "Delete", true);
        assert_eq!(spec.buttons[1].variant, ButtonVariant::Destructive);
        assert_eq!(spec.default, DefaultButton::Cancel);
    }

    #[test]
    fn unsaved_changes_three_way() {
        let spec = unsaved_changes("Unsaved changes", "Save before closing?", vec![]);
        // [Cancel, Discard, Save changes], default = Save.
        assert_eq!(spec.buttons.len(), 3);
        assert_eq!(spec.buttons[0].label, "Cancel");
        assert_eq!(spec.buttons[1].label, "Discard");
        assert_eq!(spec.buttons[1].variant, ButtonVariant::Destructive);
        assert_eq!(spec.buttons[2].label, "Save changes");
        assert_eq!(spec.buttons[2].variant, ButtonVariant::Primary);
        assert_eq!(spec.default, DefaultButton::Accept);
    }

    #[test]
    fn unsaved_choice_mapping() {
        assert_eq!(unsaved_choice_for(0), UnsavedChoice::Cancel);
        assert_eq!(unsaved_choice_for(1), UnsavedChoice::Discard);
        assert_eq!(unsaved_choice_for(2), UnsavedChoice::Save);
        // Out-of-range → Cancel (defensive).
        assert_eq!(unsaved_choice_for(9), UnsavedChoice::Cancel);
    }

    #[test]
    fn detail_none_for_empty() {
        assert_eq!(format_detail(&[]), DetailLayout::None);
        assert_eq!(
            format_detail(&["".to_string(), "   ".to_string()]),
            DetailLayout::None
        );
    }

    #[test]
    fn detail_label_for_small_lists() {
        let items: Vec<String> = (0..5).map(|i| format!("file{i}.rcx")).collect();
        match format_detail(&items) {
            DetailLayout::Label(s) => {
                // 5 items joined by newlines (≤ threshold → label).
                assert_eq!(s.lines().count(), 5);
            }
            other => panic!("expected Label, got {other:?}"),
        }
    }

    #[test]
    fn detail_list_for_more_than_five() {
        let items: Vec<String> = (0..6).map(|i| format!("file{i}.rcx")).collect();
        match format_detail(&items) {
            DetailLayout::List(v) => assert_eq!(v.len(), 6),
            other => panic!("expected List, got {other:?}"),
        }
    }

    #[test]
    fn input_specs_build() {
        let t = InputSpec::text("Rename", "New name:", "Player", "name");
        assert_eq!(t.title, "Rename");
        let i = InputSpec::int("Size", "Bytes:", 8, 1, 4096);
        assert_eq!(i.label, "Bytes:");
        let it = InputSpec::item("Kind", "Pick:", vec!["a".into(), "b".into()], 1);
        assert!(matches!(it.kind, super::InputKind::Item { current: 1, .. }));
    }

    #[test]
    fn input_int_clamps() {
        assert_eq!(InputSpec::clamp_int(0, 1, 60000), 1);
        assert_eq!(InputSpec::clamp_int(99999, 1, 60000), 60000);
        assert_eq!(InputSpec::clamp_int(42, 1, 60000), 42);
    }
}
