//! `Theme` model — port of `src/themes/theme.h` and `theme.cpp`.
//!
//! A `Theme` is a named bundle of 31 colors. Each color is `Option<Color>`:
//! the `Option` is the `QColor::isValid()` sentinel the C++ `from_json`
//! pipeline branches on. The flat JSON shape `{ "name": str, "<key>":
//! "#rrggbb", ... }` is handled by hand-written [`Theme::to_json`] /
//! [`Theme::from_json`] driven by [`THEME_FIELDS`] — one source of truth that
//! mirrors the C++ `kThemeFields` loop. We do NOT derive serde for `Theme`
//! because the key names are camelCase, the values are hex strings, and
//! `from_json` applies a derivation pipeline plain derive cannot express.

use serde_json::{Map, Value};

use super::color::{hex_or_black, lerp_rgb, Color};

/// Identifier for each color field — replaces the C++ pointer-to-member
/// `QColor Theme::*ptr` used by `kThemeFields` (`theme.cpp:9-42`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FieldId {
    Background,
    BackgroundAlt,
    Surface,
    Border,
    BorderFocused,
    Button,
    Text,
    TextDim,
    TextMuted,
    TextFaint,
    Hover,
    Selected,
    Selection,
    SyntaxKeyword,
    SyntaxNumber,
    SyntaxString,
    SyntaxComment,
    SyntaxPreproc,
    SyntaxType,
    IndHoverSpan,
    IndCmdPill,
    IndDataChanged,
    IndHeatCold,
    IndHeatWarm,
    IndHeatHot,
    IndHintGreen,
    IndRttiHint,
    MarkerPtr,
    MarkerCycle,
    MarkerError,
    FocusGlow,
}

/// Shared field metadata for serialization + the editor UI — replaces
/// `struct ThemeFieldMeta` (`theme.h:62-67`).
pub struct ThemeFieldMeta {
    /// JSON key (camelCase).
    pub key: &'static str,
    /// Display label (editor row).
    pub label: &'static str,
    /// Section group name (editor section header).
    pub group: &'static str,
    /// Field selector.
    pub id: FieldId,
}

/// The field table, in exact `kThemeFields` source order (`theme.cpp:9-42`).
/// Order is load-bearing for the editor row layout / section grouping.
/// `kThemeFieldCount` == 31.
pub const THEME_FIELDS: &[ThemeFieldMeta] = &[
    ThemeFieldMeta {
        key: "background",
        label: "Background",
        group: "Chrome",
        id: FieldId::Background,
    },
    ThemeFieldMeta {
        key: "backgroundAlt",
        label: "Background Alt",
        group: "Chrome",
        id: FieldId::BackgroundAlt,
    },
    ThemeFieldMeta {
        key: "surface",
        label: "Surface",
        group: "Chrome",
        id: FieldId::Surface,
    },
    ThemeFieldMeta {
        key: "border",
        label: "Border",
        group: "Chrome",
        id: FieldId::Border,
    },
    ThemeFieldMeta {
        key: "borderFocused",
        label: "Border Focused",
        group: "Chrome",
        id: FieldId::BorderFocused,
    },
    ThemeFieldMeta {
        key: "button",
        label: "Button",
        group: "Chrome",
        id: FieldId::Button,
    },
    ThemeFieldMeta {
        key: "text",
        label: "Text",
        group: "Text",
        id: FieldId::Text,
    },
    ThemeFieldMeta {
        key: "textDim",
        label: "Text Dim",
        group: "Text",
        id: FieldId::TextDim,
    },
    ThemeFieldMeta {
        key: "textMuted",
        label: "Text Muted",
        group: "Text",
        id: FieldId::TextMuted,
    },
    ThemeFieldMeta {
        key: "textFaint",
        label: "Text Faint",
        group: "Text",
        id: FieldId::TextFaint,
    },
    ThemeFieldMeta {
        key: "hover",
        label: "Hover",
        group: "Interactive",
        id: FieldId::Hover,
    },
    ThemeFieldMeta {
        key: "selected",
        label: "Selected",
        group: "Interactive",
        id: FieldId::Selected,
    },
    ThemeFieldMeta {
        key: "selection",
        label: "Selection",
        group: "Interactive",
        id: FieldId::Selection,
    },
    ThemeFieldMeta {
        key: "syntaxKeyword",
        label: "Keyword",
        group: "Syntax",
        id: FieldId::SyntaxKeyword,
    },
    ThemeFieldMeta {
        key: "syntaxNumber",
        label: "Number",
        group: "Syntax",
        id: FieldId::SyntaxNumber,
    },
    ThemeFieldMeta {
        key: "syntaxString",
        label: "String",
        group: "Syntax",
        id: FieldId::SyntaxString,
    },
    ThemeFieldMeta {
        key: "syntaxComment",
        label: "Comment",
        group: "Syntax",
        id: FieldId::SyntaxComment,
    },
    ThemeFieldMeta {
        key: "syntaxPreproc",
        label: "Preprocessor",
        group: "Syntax",
        id: FieldId::SyntaxPreproc,
    },
    ThemeFieldMeta {
        key: "syntaxType",
        label: "Type",
        group: "Syntax",
        id: FieldId::SyntaxType,
    },
    ThemeFieldMeta {
        key: "indHoverSpan",
        label: "Hover Span",
        group: "Indicators",
        id: FieldId::IndHoverSpan,
    },
    ThemeFieldMeta {
        key: "indCmdPill",
        label: "Cmd Pill",
        group: "Indicators",
        id: FieldId::IndCmdPill,
    },
    ThemeFieldMeta {
        key: "indDataChanged",
        label: "Data Changed",
        group: "Indicators",
        id: FieldId::IndDataChanged,
    },
    ThemeFieldMeta {
        key: "indHeatCold",
        label: "Heat Cold",
        group: "Indicators",
        id: FieldId::IndHeatCold,
    },
    ThemeFieldMeta {
        key: "indHeatWarm",
        label: "Heat Warm",
        group: "Indicators",
        id: FieldId::IndHeatWarm,
    },
    ThemeFieldMeta {
        key: "indHeatHot",
        label: "Heat Hot",
        group: "Indicators",
        id: FieldId::IndHeatHot,
    },
    ThemeFieldMeta {
        key: "indHintGreen",
        label: "Hint Green",
        group: "Indicators",
        id: FieldId::IndHintGreen,
    },
    ThemeFieldMeta {
        key: "indRttiHint",
        label: "RTTI Hint",
        group: "Indicators",
        id: FieldId::IndRttiHint,
    },
    ThemeFieldMeta {
        key: "markerPtr",
        label: "Pointer",
        group: "Markers",
        id: FieldId::MarkerPtr,
    },
    ThemeFieldMeta {
        key: "markerCycle",
        label: "Cycle",
        group: "Markers",
        id: FieldId::MarkerCycle,
    },
    ThemeFieldMeta {
        key: "markerError",
        label: "Error",
        group: "Markers",
        id: FieldId::MarkerError,
    },
    ThemeFieldMeta {
        key: "focusGlow",
        label: "Focus Glow",
        group: "Presentation",
        id: FieldId::FocusGlow,
    },
];

/// `struct Theme` (`theme.h:8-58`). `Default` (all `None`, empty name) equals
/// the C++ default-constructed `Theme` (the `static const Theme empty`
/// fallback in `ThemeManager::current()`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Theme {
    pub name: String,
    // ── Chrome ──
    pub background: Option<Color>,
    pub background_alt: Option<Color>,
    pub surface: Option<Color>,
    pub border: Option<Color>,
    pub border_focused: Option<Color>,
    pub button: Option<Color>,
    // ── Text ──
    pub text: Option<Color>,
    pub text_dim: Option<Color>,
    pub text_muted: Option<Color>,
    pub text_faint: Option<Color>,
    // ── Interactive ──
    pub hover: Option<Color>,
    pub selected: Option<Color>,
    pub selection: Option<Color>,
    // ── Syntax ──
    pub syntax_keyword: Option<Color>,
    pub syntax_number: Option<Color>,
    pub syntax_string: Option<Color>,
    pub syntax_comment: Option<Color>,
    pub syntax_preproc: Option<Color>,
    pub syntax_type: Option<Color>,
    // ── Indicators ──
    pub ind_hover_span: Option<Color>,
    pub ind_cmd_pill: Option<Color>,
    pub ind_data_changed: Option<Color>,
    pub ind_heat_cold: Option<Color>,
    pub ind_heat_warm: Option<Color>,
    pub ind_heat_hot: Option<Color>,
    pub ind_hint_green: Option<Color>,
    pub ind_rtti_hint: Option<Color>,
    // ── Markers ──
    pub marker_ptr: Option<Color>,
    pub marker_cycle: Option<Color>,
    pub marker_error: Option<Color>,
    // ── Presentation ──
    pub focus_glow: Option<Color>,
}

impl Theme {
    /// Read a field by id — replaces `this->*ptr` (read).
    pub fn get(&self, id: FieldId) -> Option<Color> {
        use FieldId::*;
        match id {
            Background => self.background,
            BackgroundAlt => self.background_alt,
            Surface => self.surface,
            Border => self.border,
            BorderFocused => self.border_focused,
            Button => self.button,
            Text => self.text,
            TextDim => self.text_dim,
            TextMuted => self.text_muted,
            TextFaint => self.text_faint,
            Hover => self.hover,
            Selected => self.selected,
            Selection => self.selection,
            SyntaxKeyword => self.syntax_keyword,
            SyntaxNumber => self.syntax_number,
            SyntaxString => self.syntax_string,
            SyntaxComment => self.syntax_comment,
            SyntaxPreproc => self.syntax_preproc,
            SyntaxType => self.syntax_type,
            IndHoverSpan => self.ind_hover_span,
            IndCmdPill => self.ind_cmd_pill,
            IndDataChanged => self.ind_data_changed,
            IndHeatCold => self.ind_heat_cold,
            IndHeatWarm => self.ind_heat_warm,
            IndHeatHot => self.ind_heat_hot,
            IndHintGreen => self.ind_hint_green,
            IndRttiHint => self.ind_rtti_hint,
            MarkerPtr => self.marker_ptr,
            MarkerCycle => self.marker_cycle,
            MarkerError => self.marker_error,
            FocusGlow => self.focus_glow,
        }
    }

    /// Write a field by id — replaces `this->*ptr` (write).
    pub fn set(&mut self, id: FieldId, c: Option<Color>) {
        use FieldId::*;
        match id {
            Background => self.background = c,
            BackgroundAlt => self.background_alt = c,
            Surface => self.surface = c,
            Border => self.border = c,
            BorderFocused => self.border_focused = c,
            Button => self.button = c,
            Text => self.text = c,
            TextDim => self.text_dim = c,
            TextMuted => self.text_muted = c,
            TextFaint => self.text_faint = c,
            Hover => self.hover = c,
            Selected => self.selected = c,
            Selection => self.selection = c,
            SyntaxKeyword => self.syntax_keyword = c,
            SyntaxNumber => self.syntax_number = c,
            SyntaxString => self.syntax_string = c,
            SyntaxComment => self.syntax_comment = c,
            SyntaxPreproc => self.syntax_preproc = c,
            SyntaxType => self.syntax_type = c,
            IndHoverSpan => self.ind_hover_span = c,
            IndCmdPill => self.ind_cmd_pill = c,
            IndDataChanged => self.ind_data_changed = c,
            IndHeatCold => self.ind_heat_cold = c,
            IndHeatWarm => self.ind_heat_warm = c,
            IndHeatHot => self.ind_heat_hot = c,
            IndHintGreen => self.ind_hint_green = c,
            IndRttiHint => self.ind_rtti_hint = c,
            MarkerPtr => self.marker_ptr = c,
            MarkerCycle => self.marker_cycle = c,
            MarkerError => self.marker_error = c,
            FocusGlow => self.focus_glow = c,
        }
    }

    /// `Theme::toJson()` (`theme.cpp:44-50`).
    ///
    /// Writes `"name"` then every field's hex (`None` → `"#000000"`, matching
    /// `QColor().name()`). Key order is not load-bearing: Qt re-sorts
    /// `QJsonObject` keys alphabetically on write; `serde_json::Map` (the
    /// default `BTreeMap` backing, no `preserve_order`) emits sorted keys,
    /// which incidentally matches Qt's alphabetical output.
    pub fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("name".to_string(), Value::String(self.name.clone()));
        for f in THEME_FIELDS {
            o.insert(
                f.key.to_string(),
                Value::String(hex_or_black(self.get(f.id))),
            );
        }
        Value::Object(o)
    }

    /// `Theme::fromJson(obj)` (`theme.cpp:52-108`).
    ///
    /// Lenient (never fails): bad / missing values become `None`, a bad `name`
    /// becomes `"Untitled"`. Then applies the §2.4 derivation pipeline (heat
    /// gradient, focusGlow, RTTI hint, hover-distinctness).
    pub fn from_json(o: &Value) -> Theme {
        let mut t = Theme::default();

        // name: absent OR non-string → "Untitled" (Qt `o["name"].toString("Untitled")`).
        t.name = o
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Untitled")
            .to_string();

        // Each present key parses; non-string → "" → parse None; unparseable → None.
        for f in THEME_FIELDS {
            if let Some(v) = o.get(f.key) {
                let s = v.as_str().unwrap_or("");
                t.set(f.id, Color::parse(s));
            }
            // keys NOT present stay None
        }

        // ── Derivation pipeline (theme.cpp:59-106) ──

        // 1. Heat amber gradient (theme.cpp:62-74).
        //    cold = dim nudged 30% toward warm gold; warm = dim nudged 60%
        //    toward orange; hot = the theme's markerPtr (copied directly — may
        //    be None for a sparse theme, mirroring `t.indHeatHot = t.markerPtr`
        //    where markerPtr is an invalid QColor).
        let dim = t.text_dim.unwrap_or(Color::rgb(133, 133, 133));
        if t.ind_heat_cold.is_none() {
            t.ind_heat_cold = Some(lerp_rgb(dim, Color::rgb(210, 170, 100), 0.30));
        }
        if t.ind_heat_warm.is_none() {
            t.ind_heat_warm = Some(lerp_rgb(dim, Color::rgb(235, 145, 50), 0.60));
        }
        if t.ind_heat_hot.is_none() {
            t.ind_heat_hot = t.marker_ptr;
        }

        // 2. focusGlow (theme.cpp:82-83).
        if t.focus_glow.is_none() {
            t.focus_glow = Some(
                t.border_focused
                    .unwrap_or_else(|| Color::parse("#4fc3f7").unwrap()),
            );
        }

        // 3. indRttiHint (theme.cpp:88-89).
        if t.ind_rtti_hint.is_none() {
            t.ind_rtti_hint = Some(Color::parse("#d19a66").unwrap());
        }

        // 4. Hover-distinctness guard (theme.cpp:86-92): only if BOTH hover
        //    and background are Some.
        if let (Some(h), Some(bg)) = (t.hover, t.background) {
            let dist = (h.r as i32 - bg.r as i32).abs()
                + (h.g as i32 - bg.g as i32).abs()
                + (h.b as i32 - bg.b as i32).abs();
            if dist < 20 {
                t.hover = Some(bg.lighter_130());
            }
        }

        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_table_count_is_31() {
        assert_eq!(THEME_FIELDS.len(), 31); // ≙ kThemeFieldCount
    }

    #[test]
    fn get_set_roundtrip_all_fields() {
        let mut t = Theme::default();
        for f in THEME_FIELDS {
            assert_eq!(t.get(f.id), None);
            t.set(f.id, Some(Color::rgb(1, 2, 3)));
            assert_eq!(t.get(f.id), Some(Color::rgb(1, 2, 3)));
        }
    }

    #[test]
    fn to_json_emits_black_for_none() {
        let t = Theme::default();
        let v = t.to_json();
        let o = v.as_object().unwrap();
        assert_eq!(o["name"], Value::String(String::new()));
        for f in THEME_FIELDS {
            assert_eq!(o[f.key], Value::String("#000000".into()), "{}", f.key);
        }
        // Exactly name + 31 keys.
        assert_eq!(o.len(), 32);
    }

    #[test]
    fn from_json_name_fallback() {
        // Absent name → "Untitled".
        let v = serde_json::json!({ "background": "#ff0000" });
        assert_eq!(Theme::from_json(&v).name, "Untitled");
        // Non-string name → "Untitled".
        let v = serde_json::json!({ "name": 42 });
        assert_eq!(Theme::from_json(&v).name, "Untitled");
        // Present string name kept.
        let v = serde_json::json!({ "name": "Foo" });
        assert_eq!(Theme::from_json(&v).name, "Foo");
    }

    #[test]
    fn from_json_missing_fields() {
        // C++ test_theme.cpp:78-90 (fromJsonMissingFields).
        let v = serde_json::json!({ "name": "Sparse", "background": "#ff0000" });
        let t = Theme::from_json(&v);
        assert_eq!(t.name, "Sparse");
        assert_eq!(t.background, Some(Color::rgb(0xff, 0, 0)));
        // No fallback for text / syntaxKeyword → stay None.
        assert!(t.text.is_none());
        assert!(t.syntax_keyword.is_none());
        // C++ has no marker fallbacks: a sparse theme leaves all three markers
        // invalid (test_theme.cpp:89 `QVERIFY(!t.markerError.isValid())`).
        assert!(t.marker_error.is_none());
        assert!(t.marker_ptr.is_none());
        assert!(t.marker_cycle.is_none());
    }

    #[test]
    fn heat_derivation() {
        // C++ theme.cpp:62-74. With Long Night's textDim #7F7E74 and an explicit
        // markerPtr: cold = lerp(dim, (210,170,100), 0.30), warm = lerp(dim,
        // (235,145,50), 0.60), hot = markerPtr.
        let v = serde_json::json!({ "name": "T", "textDim": "#7F7E74", "markerPtr": "#FF5370" });
        let t = Theme::from_json(&v);
        let dim = Color::rgb(0x7f, 0x7e, 0x74);
        assert_eq!(
            t.ind_heat_cold,
            Some(lerp_rgb(dim, Color::rgb(210, 170, 100), 0.30))
        );
        assert_eq!(t.ind_heat_cold, Color::parse("#978b70")); // golden value
        assert_eq!(
            t.ind_heat_warm,
            Some(lerp_rgb(dim, Color::rgb(235, 145, 50), 0.60))
        );
        assert_eq!(t.ind_heat_warm, Color::parse("#bf894d")); // golden value
                                                              // hot = the theme's markerPtr, copied directly.
        assert_eq!(t.ind_heat_hot, Color::parse("#FF5370"));

        // markerPtr absent → ind_heat_hot stays None (`t.indHeatHot = t.markerPtr`
        // where markerPtr is an invalid QColor).
        let v = serde_json::json!({ "name": "T", "textDim": "#7F7E74" });
        let t = Theme::from_json(&v);
        assert!(t.ind_heat_hot.is_none());

        // Absent textDim → dim = (133,133,133): cold #9c907c, warm #c28c54.
        let v = serde_json::json!({ "name": "T" });
        let t = Theme::from_json(&v);
        let dim = Color::rgb(133, 133, 133);
        assert_eq!(
            t.ind_heat_cold,
            Some(lerp_rgb(dim, Color::rgb(210, 170, 100), 0.30))
        );
        assert_eq!(t.ind_heat_cold, Color::parse("#9c907c"));
        assert_eq!(
            t.ind_heat_warm,
            Some(lerp_rgb(dim, Color::rgb(235, 145, 50), 0.60))
        );
        assert_eq!(t.ind_heat_warm, Color::parse("#c28c54"));
    }

    #[test]
    fn focus_glow_derivation() {
        // border_focused present, focusGlow absent → focus_glow == border_focused.
        let v = serde_json::json!({ "name": "T", "borderFocused": "#888888" });
        assert_eq!(Theme::from_json(&v).focus_glow, Color::parse("#888888"));
        // Neither → #4fc3f7.
        let v = serde_json::json!({ "name": "T" });
        assert_eq!(Theme::from_json(&v).focus_glow, Color::parse("#4fc3f7"));
        // Explicit focusGlow kept (phosphor ships #28cdb2).
        let v =
            serde_json::json!({ "name": "T", "borderFocused": "#888888", "focusGlow": "#28cdb2" });
        assert_eq!(Theme::from_json(&v).focus_glow, Color::parse("#28cdb2"));
    }

    #[test]
    fn hover_distinctness_guard() {
        // dist = 3 < 20 → hover replaced by background.lighter_130().
        let v = serde_json::json!({ "name": "T", "background": "#202020", "hover": "#212121" });
        let t = Theme::from_json(&v);
        assert_eq!(t.hover, Some(Color::rgb(0x20, 0x20, 0x20).lighter_130()));

        // dist >= 20 → hover unchanged.
        let v = serde_json::json!({ "name": "T", "background": "#202020", "hover": "#303030" });
        let t = Theme::from_json(&v);
        assert_eq!(t.hover, Color::parse("#303030"));

        // Only fires when BOTH present: hover present, background absent → unchanged.
        let v = serde_json::json!({ "name": "T", "hover": "#212121" });
        let t = Theme::from_json(&v);
        assert_eq!(t.hover, Color::parse("#212121"));
    }

    #[test]
    fn from_json_unparseable_hex_is_none() {
        let v = serde_json::json!({ "name": "T", "text": "garbage", "syntaxNumber": 5 });
        let t = Theme::from_json(&v);
        assert!(t.text.is_none());
        assert!(t.syntax_number.is_none());
    }
}
