//! Bottom status bar — **STUB**.
//!
//! Port target: the C++ status bar (app-shell §11) — the bottom strip with the
//! active-source readout, selection/offset info, and the `ShimmerLabel` (a small
//! custom shimmer `Element`; see the cookbook "Status bar" gap). Built from
//! plain `div`/flex + `Label` over the chrome background.
//!
//! Pre-declared here so the chrome surface stage can fill it in without editing
//! `src/ui/mod.rs`. It follows the shared Zed design system in
//! [`crate::ui::design`] (`_design/zed_ui_spec.md` → "status-bar"). Empty for
//! now; gated behind the `ui` feature.
