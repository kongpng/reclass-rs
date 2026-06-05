//! A modal **plugin dialog** (design §3 `Contribution::Dialog`, §6 Phase 2) — the
//! generalized C++ `selectTarget`. Renders a plugin's declarative
//! [`ViewTree`](crate::plugin::view::ViewTree) inside the shared Zed modal
//! scaffolding ([`crate::ui::dialogs::modal`]) and routes its events back to the
//! plugin Elm-style.
//!
//! Like [`PluginPanel`](crate::ui::pluginpanel) it is **manager-agnostic**: it
//! owns the current tree + persistent inputs, and re-emits widget
//! [`UiEvent`](crate::plugin::view::UiEvent)s + the dialog outcome as
//! [`PluginDialogEvent`] for the window (the `PluginManager` owner) to route. On
//! Submit it gathers the current text-input values into the
//! [`DialogResult`](crate::plugin::contract::DialogResult) field set (so the
//! plugin's `handle_dialog_closed` receives the chosen target — the C++
//! `selectTarget` return). The proven `window.open_dialog` + subscription mount
//! pattern (see [`crate::ui::processpicker`]) carries it into the modal layer.
//!
//! Gated behind the `ui` feature.

use gpui::*;

use crate::plugin::contract::DialogResult;
use crate::plugin::view::{UiEvent, ViewTree};
use crate::ui::dialogs::modal;
use crate::ui::pluginview::{render_view_tree, PluginRenderState};

/// What a [`PluginDialog`] raises for the window to route through the
/// `PluginManager`.
#[derive(Clone, Debug)]
pub enum PluginDialogEvent {
    /// A widget inside the dialog emitted an event (Elm input). The window
    /// forwards it to `PluginManager::handle_ui_event` and pushes any fresh tree
    /// back via [`PluginDialog::set_tree`].
    Ui { view_id: String, event: UiEvent },
    /// The dialog finished (Submit / Cancel / Esc) — the window reports the
    /// [`DialogResult`] to the plugin via `PluginManager::handle_dialog_closed`
    /// and dismisses the modal. This is the generalized `selectTarget` return.
    Closed {
        view_id: String,
        result: DialogResult,
    },
}

/// A modal rendering a plugin's [`ViewTree`] (design §3 `Dialog`).
pub struct PluginDialog {
    view_id: String,
    title: String,
    tree: ViewTree,
    render_state: PluginRenderState,
    focus_handle: FocusHandle,
}

impl PluginDialog {
    /// Build a dialog for the contributed `(view_id, title, initial_tree)`.
    pub fn new(
        view_id: impl Into<String>,
        title: impl Into<String>,
        initial: ViewTree,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut render_state = PluginRenderState::new();
        render_state.sync(&initial, window, cx);
        PluginDialog {
            view_id: view_id.into(),
            title: title.into(),
            tree: initial,
            render_state,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Construct as an [`Entity`].
    pub fn view(
        view_id: impl Into<String>,
        title: impl Into<String>,
        initial: ViewTree,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let view_id = view_id.into();
        let title = title.into();
        cx.new(|cx| PluginDialog::new(view_id, title, initial, window, cx))
    }

    /// The contributing dialog's id (routing key).
    pub fn view_id(&self) -> &str {
        &self.view_id
    }

    /// The current tree (test hook / wiring).
    pub fn tree(&self) -> &ViewTree {
        &self.tree
    }

    /// Replace the tree (the Elm update result the window pushes back). Re-syncs
    /// the persistent input cache.
    pub fn set_tree(&mut self, tree: ViewTree, window: &mut Window, cx: &mut Context<Self>) {
        self.render_state.sync(&tree, window, cx);
        self.tree = tree;
        cx.notify();
    }

    /// Re-emit a widget [`UiEvent`] as a [`PluginDialogEvent::Ui`].
    fn emit_event(&mut self, event: UiEvent, cx: &mut Context<Self>) {
        cx.emit(PluginDialogEvent::Ui {
            view_id: self.view_id.clone(),
            event,
        });
    }

    /// Submit the dialog: gather the current input values into a
    /// [`DialogResult::Submitted`] and emit [`PluginDialogEvent::Closed`] (the
    /// `selectTarget` accept).
    fn submit(&mut self, cx: &mut Context<Self>) {
        let values = self.render_state.input_values(cx);
        cx.emit(PluginDialogEvent::Closed {
            view_id: self.view_id.clone(),
            result: DialogResult::Submitted { values },
        });
    }

    /// Cancel the dialog: emit [`PluginDialogEvent::Closed`] with
    /// [`DialogResult::Cancelled`] (the `selectTarget` reject / Esc).
    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(PluginDialogEvent::Closed {
            view_id: self.view_id.clone(),
            result: DialogResult::Cancelled,
        });
    }
}

impl EventEmitter<PluginDialogEvent> for PluginDialog {}

impl Focusable for PluginDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PluginDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let view_id = self.view_id.clone();
        let on_event = move |ev: UiEvent, _window: &mut Window, cx: &mut App| {
            if let Some(dialog) = view.upgrade() {
                dialog.update(cx, |this, cx| this.emit_event(ev, cx));
            }
        };
        let content = render_view_tree(&self.tree, &view_id, on_event, &self.render_state, cx);

        // Clamp a comfortable 560×440 card to the live window (same as the other
        // modals) so the footer stays on-screen.
        let card_w = modal::clamp_width(560., window);
        let card_h = modal::clamp_height(440., 64., window);

        let body = modal::body(cx).child(content);

        let footer = modal::footer(cx)
            .child(
                gpui_component::button::Button::new("plugin-dialog-cancel")
                    .label("Cancel")
                    .on_click(cx.listener(|this, _e, _w, cx| this.cancel(cx))),
            )
            .child({
                use gpui_component::button::ButtonVariants as _;
                gpui_component::button::Button::new("plugin-dialog-submit")
                    .primary()
                    .label("Submit")
                    .on_click(cx.listener(|this, _e, _w, cx| this.submit(cx)))
            });

        modal::card(cx)
            .id(SharedString::from(format!("rcx-plugin-dialog-{view_id}")))
            .track_focus(&self.focus_handle)
            .key_context("RcxPluginDialog")
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
                if ev.keystroke.key.as_str() == "escape" {
                    this.cancel(cx);
                    cx.stop_propagation();
                }
            }))
            .w(card_w)
            .h(card_h)
            .child(
                modal::header_with_close(self.title.clone(), "plugin-dialog-close", cx.listener(|this, _e, _w, cx| this.cancel(cx)), cx),
            )
            .child(body)
            .child(footer)
    }
}

#[cfg(test)]
mod tests {
    // The dialog's effectful logic (submit gathers input values, cancel) is
    // gpui-bound; the pure ViewTree→widget mapping is tested in
    // `crate::ui::pluginview` and the end-to-end Attach→set_data_source→close
    // flow in `crate::plugin::demo` (via `MockPluginHost`). Here we assert the
    // gpui-free event payload shapes so a refactor of `PluginDialogEvent` /
    // `DialogResult` is caught.
    //
    // NOTE: import specific types (not `use super::*`) so this test module does
    // NOT re-glob `gpui::*` into its codegen scope — that tripped a rustc
    // `--test` codegen SIGSEGV on this large crate.
    use super::PluginDialogEvent;
    use crate::plugin::contract::DialogResult;
    use crate::plugin::view::UiEvent;

    #[test]
    fn dialog_event_ui_and_closed_shapes() {
        let ui = PluginDialogEvent::Ui {
            view_id: "demo.target".to_string(),
            event: UiEvent::Clicked("attach".to_string()),
        };
        match ui {
            PluginDialogEvent::Ui { view_id, event } => {
                assert_eq!(view_id, "demo.target");
                assert_eq!(event, UiEvent::Clicked("attach".to_string()));
            }
            _ => panic!("expected Ui"),
        }

        let closed = PluginDialogEvent::Closed {
            view_id: "demo.target".to_string(),
            result: DialogResult::Submitted {
                values: vec![("target".to_string(), "1:x".to_string())],
            },
        };
        match closed {
            PluginDialogEvent::Closed { view_id, result } => {
                assert_eq!(view_id, "demo.target");
                assert!(result.is_submitted());
                assert_eq!(result.get("target"), Some("1:x"));
            }
            _ => panic!("expected Closed"),
        }
    }
}
