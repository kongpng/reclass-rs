//! GPUI views — the bespoke editor `Element` (stub) + standard chrome via
//! gpui-component, theme application, and the command palette.
//!
//! Port of `src/editor.*`, `widgets/*`, dialogs, popups, docks, `mainwindow.h`,
//! `startpage.h`, `titlebar.*`. **SKELETON** — only the minimal window
//! (gpui-component `init` → `Root` → a `Button`/dock) is wired up, per
//! `gpui_component_cookbook.md`. The bespoke raw-gpui structured-editor surface
//! is a documented stub (`editor`). Gated behind the `ui` feature.

pub mod editor;

use gpui::*;
use gpui_component::{
    button::{Button, ButtonVariants},
    ActiveTheme, Root,
};

/// The application's root view. Built per the gpui-component setup pattern
/// (`gpui_component_cookbook.md` §4): wrapped in [`Root`] so overlays / modals /
/// notifications / tooltips render. **SKELETON** — a placeholder workspace; the
/// real `MainWindow` (titlebar, dock area, doc tabs, panels) is ported by the
/// `ui` workflow (ARCHITECTURE.md §9).
pub struct MainWindow;

impl Render for MainWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .size_full()
            .items_center()
            .justify_center()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child("Reclass — Rust + GPUI port (skeleton)")
            .child(
                Button::new("new-class")
                    .primary()
                    .label("New Class")
                    .on_click(|_, _, _| tracing::info!("New Class clicked (stub)")),
            )
    }
}

/// Open the main application window. Called from `main.rs` inside
/// `gpui_platform::application().run(...)`. Mirrors the cookbook's verified
/// `open_window` → `Root::new` flow.
pub fn open_main_window(cx: &mut App) {
    cx.spawn(async move |cx| {
        cx.open_window(WindowOptions::default(), |window, cx| {
            let view = cx.new(|_| MainWindow);
            cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
        })
        .expect("failed to open window");
    })
    .detach();
}
