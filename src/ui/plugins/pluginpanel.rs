//! A dockable **plugin panel** (design §3 `Contribution::Panel`, §6 Phase 2) —
//! the host-side view that mounts a plugin's declarative
//! [`ViewTree`](crate::plugin::view::ViewTree) into a gpui-component dock
//! [`Panel`](gpui_component::dock::Panel), Zed-styled via
//! [`crate::ui::plugins::pluginview::render_view_tree`].
//!
//! ## Elm loop, decoupled from the manager
//!
//! The panel is intentionally **manager-agnostic**: it owns only the current
//! `ViewTree` + the per-view [`PluginRenderState`](crate::ui::plugins::pluginview::PluginRenderState)
//! (persistent text inputs). Interactive widgets emit a
//! [`UiEvent`](crate::plugin::view::UiEvent) that the panel re-emits as a
//! [`PluginPanelEvent`] (gpui `EventEmitter`); the **window** (which owns the
//! `PluginManager`) routes it through `PluginManager::handle_ui_event` and pushes
//! the returned fresh tree back via [`PluginPanel::set_tree`]. This is the same
//! "view emits intent, window resolves against the controller" shape the
//! workspace dock uses, so wiring the panel into the window is a small
//! subscription — and the panel renders + routes correctly in isolation (tested).
//!
//! Gated behind the `ui` feature.

use gpui::*;
use gpui_component::dock::{Panel, PanelEvent};

use crate::plugin::view::{UiEvent, ViewTree};
use crate::ui::design::{color, tokens};
use crate::ui::plugins::pluginview::{render_view_tree, PluginRenderState};

/// A UI event a [`PluginPanel`] raises for the window to route through the
/// `PluginManager` (the panel id + the widget event). The window forwards
/// `(view_id, event)` to `PluginManager::handle_ui_event` and pushes any fresh
/// tree back via [`PluginPanel::set_tree`].
#[derive(Clone, Debug)]
pub struct PluginPanelEvent {
    /// The contributing panel's id (so the window routes to the right plugin).
    pub view_id: String,
    /// The widget event (Elm input).
    pub event: UiEvent,
}

/// A dockable view rendering a plugin's [`ViewTree`] (design §3 `Panel`).
pub struct PluginPanel {
    /// The contributing panel's id (the routing key + element-id scope).
    view_id: String,
    /// The panel's display title (the dock tab label).
    title: String,
    /// The current declarative tree (replaced on re-render).
    tree: ViewTree,
    /// Persistent per-input state so text boxes keep focus + value across frames.
    render_state: PluginRenderState,
    focus_handle: FocusHandle,
}

impl PluginPanel {
    /// Build a panel for the contributed `(view_id, title, initial_tree)`.
    pub fn new(
        view_id: impl Into<String>,
        title: impl Into<String>,
        initial: ViewTree,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut render_state = PluginRenderState::new();
        render_state.sync(&initial, window, cx);
        PluginPanel {
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
        cx.new(|cx| PluginPanel::new(view_id, title, initial, window, cx))
    }

    /// The contributing panel's id (routing key).
    pub fn view_id(&self) -> &str {
        &self.view_id
    }

    /// The current tree (test hook / wiring).
    pub fn tree(&self) -> &ViewTree {
        &self.tree
    }

    /// Replace the tree (the Elm update result the window pushes back after
    /// routing a [`PluginPanelEvent`] through the manager). Re-syncs the
    /// persistent input cache to the new tree.
    pub fn set_tree(&mut self, tree: ViewTree, window: &mut Window, cx: &mut Context<Self>) {
        self.render_state.sync(&tree, window, cx);
        self.tree = tree;
        cx.notify();
    }

    /// Re-emit a widget [`UiEvent`] as a [`PluginPanelEvent`] for the window.
    fn emit_event(&mut self, event: UiEvent, cx: &mut Context<Self>) {
        cx.emit(PluginPanelEvent {
            view_id: self.view_id.clone(),
            event,
        });
    }
}

impl Panel for PluginPanel {
    fn panel_name(&self) -> &'static str {
        "PluginPanel"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from(self.title.clone())
    }
}

impl EventEmitter<PanelEvent> for PluginPanel {}
impl EventEmitter<PluginPanelEvent> for PluginPanel {}

impl Focusable for PluginPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PluginPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let view_id = self.view_id.clone();
        // The on_event callback routes back to this entity, which re-emits the
        // event up to the window (the manager owner).
        let on_event = move |ev: UiEvent, _window: &mut Window, cx: &mut App| {
            if let Some(panel) = view.upgrade() {
                panel.update(cx, |this, cx| this.emit_event(ev, cx));
            }
        };
        let content = render_view_tree(&self.tree, &view_id, on_event, &self.render_state, cx);

        gpui_component::v_flex()
            .id(SharedString::from(format!("plugin-panel-{view_id}")))
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(color::panel_bg(cx))
            .child(
                gpui_component::v_flex()
                    .id(SharedString::from(format!("plugin-panel-scroll-{view_id}")))
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .p(px(tokens::space::LG))
                    .overflow_y_scroll()
                    .child(content),
            )
    }
}

#[cfg(test)]
mod tests {
    // The panel's interesting logic (tree replacement, event re-emission) is
    // gpui-bound (needs a Window/Context); its pure pieces — the render mapping
    // and event derivation — are unit-tested in `crate::ui::plugins::pluginview`, and the
    // panel's end-to-end routing is exercised by the demo plugin's tests through
    // `MockPluginHost` (`crate::plugin::demo`). Here we assert only the gpui-free
    // event payload shape so a refactor of `PluginPanelEvent` is caught.
    //
    // NOTE: import the specific types (not `use super::*`) so this test module
    // does NOT re-glob `gpui::*` into its codegen scope — doing so tripped a
    // rustc `--test` codegen SIGSEGV on this large crate.
    use super::PluginPanelEvent;
    use crate::plugin::view::UiEvent;

    #[test]
    fn panel_event_carries_view_id_and_event() {
        let ev = PluginPanelEvent {
            view_id: "demo.panel".to_string(),
            event: UiEvent::Clicked("demo.refresh".to_string()),
        };
        assert_eq!(ev.view_id, "demo.panel");
        assert_eq!(ev.event, UiEvent::Clicked("demo.refresh".to_string()));
    }
}
