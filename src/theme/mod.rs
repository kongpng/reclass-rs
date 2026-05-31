//! Runtime JSON theme model + manager (→ gpui-component `ThemeRegistry`).
//!
//! Port of `src/themes/theme.h` and `thememanager.h`. **SKELETON** — JSON
//! (de)serialization and the user/built-in theme loading are filled in by the
//! dedicated `themes` workflow (ARCHITECTURE.md §9). The `Theme` color model +
//! the manager signatures are in place.

use serde::{Deserialize, Serialize};

/// An RGBA color (replaces Qt's `QColor`). Serialized as a hex string in the
/// JSON theme files.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color { r, g, b, a: 255 }
    }
}

/// `struct Theme` (`theme.h:8-56`) — a named bundle of ~31 colors. Field order
/// and names mirror the C++ exactly (the JSON keys come from this).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    // ── Chrome ──
    pub background: Color,
    pub background_alt: Color,
    pub surface: Color,
    pub border: Color,
    pub border_focused: Color,
    pub button: Color,
    // ── Text ──
    pub text: Color,
    pub text_dim: Color,
    pub text_muted: Color,
    pub text_faint: Color,
    // ── Interactive ──
    pub hover: Color,
    pub selected: Color,
    pub selection: Color,
    // ── Syntax ──
    pub syntax_keyword: Color,
    pub syntax_number: Color,
    pub syntax_string: Color,
    pub syntax_comment: Color,
    pub syntax_preproc: Color,
    pub syntax_type: Color,
    // ── Indicators ──
    pub ind_hover_span: Color,
    pub ind_cmd_pill: Color,
    pub ind_data_changed: Color,
    pub ind_heat_cold: Color,
    pub ind_heat_warm: Color,
    pub ind_heat_hot: Color,
    pub ind_hint_green: Color,
    pub ind_rtti_hint: Color,
    // ── Markers ──
    pub marker_ptr: Color,
    pub marker_cycle: Color,
    pub marker_error: Color,
    // ── Presentation ──
    pub focus_glow: Color,
}

impl Theme {
    /// `Theme::toJson()` (`theme.h:58`). SKELETON.
    pub fn to_json(&self) -> serde_json::Value {
        todo!("port theme.cpp Theme::toJson (workflow: themes)")
    }
    /// `Theme::fromJson(obj)` (`theme.h:59`). SKELETON.
    pub fn from_json(_obj: &serde_json::Value) -> Theme {
        todo!("port theme.cpp Theme::fromJson (workflow: themes)")
    }
}

/// `class ThemeManager` (`thememanager.h:8-48`) — built-in + user themes, the
/// current selection, and JSON persistence. A Qt singleton in C++; modeled as
/// an owned manager here. SKELETON.
#[derive(Default)]
pub struct ThemeManager {
    built_in: Vec<Theme>,
    user: Vec<Theme>,
    current_idx: i32,
}

impl ThemeManager {
    pub fn new() -> Self {
        ThemeManager::default()
    }

    /// `themes()` (`thememanager.h:13`) — built-in followed by user themes.
    pub fn themes(&self) -> Vec<Theme> {
        self.built_in
            .iter()
            .chain(self.user.iter())
            .cloned()
            .collect()
    }
    /// `currentIndex()` (`thememanager.h:14`).
    pub fn current_index(&self) -> i32 {
        self.current_idx
    }

    /// `loadBuiltInThemes()` (`thememanager.h:43`). SKELETON.
    pub fn load_built_in_themes(&mut self) {
        todo!("port thememanager.cpp loadBuiltInThemes (workflow: themes)")
    }
    /// `loadUserThemes()` (`thememanager.h:22`). SKELETON.
    pub fn load_user_themes(&mut self) {
        todo!("port thememanager.cpp loadUserThemes (workflow: themes)")
    }
}
