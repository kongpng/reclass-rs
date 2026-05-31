//! In-window menu bar (the frameless titlebar menu row) — **STUB**.
//!
//! Port target: the C++ `createMenus` / titlebar menu row (app-shell §7) plus
//! the Linux tool-button-mirrored menus (`titlebar.cpp`). On Win/Linux this is
//! gpui-component's [`AppMenuBar`](gpui_component::menu) + `PopupMenu`; on macOS
//! the native `cx.set_menus` path (cookbook §Chrome/menus).
//!
//! Pre-declared here so the chrome surface stage can fill it in without editing
//! `src/ui/mod.rs`. It follows the shared Zed design system in
//! [`crate::ui::design`] (`_design/zed_ui_spec.md` → "menus/popovers"). Empty
//! for now; gated behind the `ui` feature.
