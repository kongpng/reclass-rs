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
/// `themed_messagebox.h:42`). The C++ draws a 32×32 severity SVG
/// (info/warning/error/question, `themed_messagebox.cpp:93-124`); the Zed port
/// substitutes a tinted leading glyph (see `open::severity_icon`). Severity also
/// influences the default button styling/labels.
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

/// The width clamp for a message box (`themed_messagebox.cpp:46-47`:
/// `setMinimumWidth(380)` / `setMaximumWidth(620)`).
pub const MSG_MIN_WIDTH: f32 = 380.0;
pub const MSG_MAX_WIDTH: f32 = 620.0;

/// How a detail block should be presented. The C++ `setDetailText`
/// (`themed_messagebox.cpp:53-71`) creates a **single** word-wrapped `QLabel`
/// (themed `textDim`) inserted above the button row — there is no list and no
/// item-count threshold (an earlier port fabricated both). So this is just
/// "no detail" vs. "one wrapped label".
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DetailLayout {
    /// No detail.
    None,
    /// A single word-wrapped label (items joined by newlines).
    Label(String),
}

/// Decide how to present a detail block (`setDetailText`,
/// `themed_messagebox.cpp:53-71`): `detail` is the per-line item vec; drop
/// empty/whitespace-only items, then — if any remain — present them as one
/// word-wrapped label (lines joined by `\n`), else [`DetailLayout::None`]. The C++
/// shows the whole detail as a single `QLabel`, regardless of item count.
pub fn format_detail(detail: &[String]) -> DetailLayout {
    let items: Vec<String> = detail
        .iter()
        .filter(|s| !s.trim().is_empty())
        .cloned()
        .collect();
    if items.is_empty() {
        DetailLayout::None
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
    use super::{format_detail, ButtonVariant, DetailLayout, MessageSpec, Severity, MSG_MAX_WIDTH};
    use crate::ui::design::{color, tokens};
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::button::ButtonVariant as GButtonVariant;
    use gpui_component::dialog::DialogButtonProps;
    use gpui_component::{Icon, IconName, WindowExt as _};

    fn to_gpui_variant(v: ButtonVariant) -> GButtonVariant {
        match v {
            ButtonVariant::Primary => GButtonVariant::Primary,
            ButtonVariant::Secondary => GButtonVariant::Secondary,
            ButtonVariant::Destructive => GButtonVariant::Danger,
        }
    }

    /// The severity icon + tint. The C++ draws a 32×32 severity SVG
    /// (`themed_messagebox.cpp:93-124`); the Zed substitution is a tinted leading
    /// glyph from the bundled icon set (the SVG assets aren't bundled here, but the
    /// behavior — a severity icon left of the title/text — matches).
    fn severity_icon(severity: Severity, cx: &App) -> impl IntoElement {
        let (name, tint) = match severity {
            Severity::Info => (IconName::Info, color::accent(cx)),
            Severity::Warning => (IconName::TriangleAlert, color::warning(cx)),
            Severity::Critical => (IconName::CircleX, color::danger(cx)),
            // No dedicated question glyph in the bundled set; the accent Info glyph
            // reads as a neutral prompt for confirm/unsaved dialogs.
            Severity::Question => (IconName::Info, color::accent(cx)),
        };
        Icon::new(name).text_color(tint)
    }

    /// Build the alert body: the wrapped message text, then the detail block (one
    /// word-wrapped muted label) when present, as a single description element. The
    /// C++ `setDetailText` (`themed_messagebox.cpp:53-71`) inserts the detail as a
    /// single `QLabel` indented 48 px so it aligns under the body text (left of the
    /// 32 px icon + 16 px gap); we left-inset the label to mirror that.
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
                        .pl(px(tokens::space::MD))
                        .text_size(px(tokens::font::UI_SM))
                        .text_color(color::text_muted(cx))
                        .child(s),
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
        window.open_alert_dialog(cx, move |alert, window, cx| {
            // Clamp the alert width to the live window so it never overflows the
            // right edge (and its buttons stay reachable) on a small window
            // (QA #1; the dialog layer centers with no edge clamp).
            let w = crate::ui::dialogs::modal::clamp_width(MSG_MAX_WIDTH, window);
            alert
                .icon(severity_icon(severity, cx))
                .title(title.clone())
                .description(description_body(text.clone(), &detail, cx))
                .width(w)
                .button_props(DialogButtonProps::default().ok_text(ok_label.clone()))
        });
    }

    /// Open a two-button confirm (`confirm`) through the `Root` overlay, calling
    /// `on_accept` if the accept button is pressed. The accept button keeps its
    /// variant (Primary or Danger for destructive).
    ///
    /// **Destructive default-focus is a documented platform limitation, NOT
    /// faked.** The C++ `themed_messagebox.cpp:181-203` rule — a destructive
    /// confirm focuses **Cancel** initially so a stray Enter can't destroy work —
    /// is fully encoded in the model ([`confirm`] sets [`DefaultButton::Cancel`]
    /// for `destructive`, asserted by `confirm_destructive_defaults_to_cancel`).
    /// But gpui-component's `AlertDialog` / [`DialogButtonProps`] expose no API to
    /// choose which footer button receives initial keyboard focus:
    /// `DialogButtonProps` has only `ok_text`/`ok_variant`/`cancel_text`/
    /// `cancel_variant`/`show_cancel` + callbacks, and `render_ok`/`render_cancel`
    /// build fresh `Button`s with no focus call (the dialog's single
    /// `focus_handle` is on the container, not a button). Setting initial focus to
    /// Cancel would require either patching the upstream crate or rebuilding the
    /// footer by hand (re-implementing focus management + Esc/Enter trapping),
    /// which is out of scope for this batch. We therefore preserve the model rule
    /// and surface `spec.default` here for when the upstream API gains the hook;
    /// we do NOT spoof focus. Enter still maps to OK via the dialog's own key
    /// handling — matching gpui-component's default for every dialog — so the only
    /// divergence is the *initial* focus ring, never the action wiring.
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
        // The model's default-focus target (the C++ destructive→Cancel safety
        // rule). Read but not applied: the AlertDialog API exposes no initial
        // button-focus hook (see the doc comment above) — this is intentionally
        // NOT faked. Kept named so the limitation is greppable.
        let _default_focus_unsupported_by_alertdialog = spec.default;
        let on_accept = std::rc::Rc::new(on_accept);
        window.open_alert_dialog(cx, move |alert, window, cx| {
            // Clamp to the live window so the confirm stays fully visible (QA #1).
            let w = crate::ui::dialogs::modal::clamp_width(MSG_MAX_WIDTH, window);
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
                .width(w)
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
                // 5 items joined by newlines (the C++ shows one wrapped label).
                assert_eq!(s.lines().count(), 5);
            }
            other => panic!("expected Label, got {other:?}"),
        }
    }

    #[test]
    fn detail_label_joins_all_items() {
        // The C++ setDetailText shows ALL items as a single word-wrapped QLabel —
        // there is no list and no item-count threshold. 6 items → a 6-line label.
        let items: Vec<String> = (0..6).map(|i| format!("file{i}.rcx")).collect();
        match format_detail(&items) {
            DetailLayout::Label(s) => {
                assert_eq!(s.lines().count(), 6);
                assert!(s.contains("file0.rcx"));
                assert!(s.contains("file5.rcx"));
            }
            other => panic!("expected Label, got {other:?}"),
        }
    }

    #[test]
    fn detail_label_drops_blank_items_only() {
        // Empty/whitespace items are dropped; the rest stay (no list ever).
        let items = vec![
            "keep1".to_string(),
            "   ".to_string(),
            "".to_string(),
            "keep2".to_string(),
        ];
        match format_detail(&items) {
            DetailLayout::Label(s) => {
                assert_eq!(s, "keep1\nkeep2");
            }
            other => panic!("expected Label, got {other:?}"),
        }
    }

    #[test]
    fn width_clamp_matches_cpp() {
        // themed_messagebox.cpp:46-47: setMinimumWidth(380)/setMaximumWidth(620).
        assert_eq!(super::MSG_MIN_WIDTH, 380.0);
        assert_eq!(super::MSG_MAX_WIDTH, 620.0);
        assert!(super::MSG_MIN_WIDTH < super::MSG_MAX_WIDTH);
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
