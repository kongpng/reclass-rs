//! `Color` — the small subset of `QColor` the theme subsystem uses.
//!
//! Port of the `QColor` operations exercised by `src/themes/theme.{h,cpp}` and
//! `thememanager.cpp`: hex parse (`QColor(QString)`), `.name()` formatting,
//! `.lighter(130)`, and the lerp helper used by the heat-gradient derivation.
//!
//! `QColor`'s validity-as-sentinel semantics are modelled as `Option<Color>`
//! at the field level (see [`crate::theme::model::Theme`]); a `Color` value
//! itself is always a concrete valid RGB triple.

/// 8-bit RGB color (replaces the RGB subset of Qt's `QColor`).
///
/// Alpha is never set or read in this subsystem, so it is not stored.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    /// Construct from raw RGB components.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color { r, g, b }
    }

    /// Parse a `#rrggbb` hex string — replaces `QColor(const QString&)`.
    ///
    /// `theme.cpp:57` constructs `QColor(o[key].toString())`. The only inputs
    /// that ever occur in this subsystem are 6-digit `#rrggbb` (all shipped
    /// JSON + editor + tests), so we accept exactly `#` + 6 hex digits
    /// (case-insensitive). Anything else yields `None` — i.e. an invalid
    /// `QColor`, which then triggers `from_json`'s fallbacks. No panic / throw,
    /// mirroring `QColor("garbage")` being silently invalid.
    pub fn parse(s: &str) -> Option<Color> {
        let bytes = s.as_bytes();
        if bytes.len() != 7 || bytes[0] != b'#' {
            return None;
        }
        let r = hex2(&s[1..3])?;
        let g = hex2(&s[3..5])?;
        let b = hex2(&s[5..7])?;
        Some(Color { r, g, b })
    }

    /// Format as `#rrggbb` (lowercase, 6 digits, no alpha) — replaces
    /// `QColor::name()` (Qt `HexRgb` default). `theme.cpp:48`.
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Replaces `QColor::lighter(130)` (`theme.cpp:105`, the hover-distinctness
    /// guard).
    ///
    /// Replicates Qt6's `QColor::lighter(factor)` for `factor = 130`
    /// (the `factor >= 100` brighten path):
    /// `toHsv()`, `v = factor*v/100`; if `v` overflows the 16-bit value channel,
    /// the excess is subtracted from saturation (Qt6 behaviour, NOT the older
    /// "set saturation to 0" path), then `fromHsv()`. Pure black stays black.
    ///
    /// Qt stores HSV internally on a 16-bit scale (`channel * 0x101`); this
    /// mirrors that so the integer value-overflow arithmetic matches. Validated
    /// against real Qt 6.10 outputs in the unit tests below.
    pub fn lighter_130(self) -> Color {
        const USHRT_MAX: i64 = 65535;

        // toHsv on the 16-bit scale (rgb16 = c * 0x101).
        let r16 = self.r as i64 * 0x101;
        let g16 = self.g as i64 * 0x101;
        let b16 = self.b as i64 * 0x101;
        let max = r16.max(g16).max(b16);
        let min = r16.min(g16).min(b16);
        let delta = max - min;

        let value = max; // 16-bit
        if delta == 0 {
            // Achromatic: hue undefined, saturation 0. lighter just scales value.
            let mut v = value * 130 / 100;
            if v > USHRT_MAX {
                v = USHRT_MAX;
            }
            let c = ((v as f64 / 65535.0) * 255.0 + 0.5) as u8;
            return Color { r: c, g: c, b: c };
        }

        // Saturation (rounded), matching Qt6's getHsvF()*65535 rounding.
        let mut sat = ((delta as f64 / max as f64) * USHRT_MAX as f64 + 0.5) as i64;

        // Hue in degrees [0, 360).
        let rf = self.r as f64 / 255.0;
        let gf = self.g as f64 / 255.0;
        let bf = self.b as f64 / 255.0;
        let df = delta as f64 / 65535.0;
        let mut hue = if max == r16 {
            (gf - bf) / df
        } else if max == g16 {
            2.0 + (bf - rf) / df
        } else {
            4.0 + (rf - gf) / df
        };
        hue *= 60.0;
        if hue < 0.0 {
            hue += 360.0;
        }

        // lighter: v = factor*v/100; on overflow, shed the excess from saturation.
        let mut v = value * 130 / 100;
        if v > USHRT_MAX {
            sat -= v - USHRT_MAX;
            if sat < 0 {
                sat = 0;
            }
            v = USHRT_MAX;
        }

        hsv16_to_rgb(hue, sat, v)
    }
}

/// HSV (16-bit s/v scale, hue in degrees) → RGB, mirroring Qt's `fromHsv`.
fn hsv16_to_rgb(hue: f64, sat: i64, value: i64) -> Color {
    if sat == 0 {
        let c = ((value as f64 / 65535.0) * 255.0 + 0.5) as u8;
        return Color { r: c, g: c, b: c };
    }
    let sf = sat as f64 / 65535.0;
    let vf = value as f64 / 65535.0;
    let hh = (if hue < 0.0 { 0.0 } else { hue }) / 60.0;
    let i = hh as i64;
    let f = hh - i as f64;
    let p = vf * (1.0 - sf);
    let q = vf * (1.0 - sf * f);
    let t = vf * (1.0 - sf * (1.0 - f));
    let (r, g, b) = match i % 6 {
        0 => (vf, t, p),
        1 => (q, vf, p),
        2 => (p, vf, t),
        3 => (p, q, vf),
        4 => (t, p, vf),
        _ => (vf, p, q),
    };
    Color {
        r: (r * 255.0 + 0.5) as u8,
        g: (g * 255.0 + 0.5) as u8,
        b: (b * 255.0 + 0.5) as u8,
    }
}

fn hex2(s: &str) -> Option<u8> {
    u8::from_str_radix(s, 16).ok()
}

/// Format `Option<Color>` for serialization: `None` → `"#000000"`.
///
/// Mirrors `QColor().name()` (an invalid/default `QColor` formats as black),
/// used by [`crate::theme::model::Theme::to_json`] and the save/path diff logic.
/// `theme.cpp:48`.
pub fn hex_or_black(c: Option<Color>) -> String {
    c.map(Color::to_hex).unwrap_or_else(|| "#000000".into())
}

/// Component-wise linear interpolation `a + int((b - a) * f)`, clamped to
/// `0..=255`. Replaces the `lerpRgb` lambda in `theme.cpp:69-73`.
///
/// The cast `(… * f) as i32` truncates toward zero, matching C++ `int(double)`.
pub fn lerp_rgb(a: Color, b: Color, f: f64) -> Color {
    let ch = |ac: u8, bc: u8| -> u8 {
        let v = ac as i32 + ((bc as i32 - ac as i32) as f64 * f) as i32;
        v.clamp(0, 255) as u8
    };
    Color {
        r: ch(a.r, b.r),
        g: ch(a.g, b.g),
        b: ch(a.b, b.b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_parse_format_roundtrip() {
        assert_eq!(Color::parse("#1e1e1e").unwrap().to_hex(), "#1e1e1e");
        // Uppercase parses, formats lowercase.
        assert_eq!(Color::parse("#1E1E1E").unwrap().to_hex(), "#1e1e1e");
        assert_eq!(
            Color::parse("#1E1E1E").unwrap(),
            Color::rgb(0x1e, 0x1e, 0x1e)
        );
        // Non-6-digit / garbage → None (we deliberately restrict to 6-digit).
        assert_eq!(Color::parse("notacolor"), None);
        assert_eq!(Color::parse("#abc"), None);
        assert_eq!(Color::parse("#abcdefff"), None);
        assert_eq!(Color::parse(""), None);
        assert_eq!(Color::parse("1e1e1e"), None);
        assert_eq!(Color::parse("#zzzzzz"), None);
        // hex_or_black.
        assert_eq!(hex_or_black(None), "#000000");
        assert_eq!(hex_or_black(Some(Color::rgb(0xff, 0, 0))), "#ff0000");
    }

    #[test]
    fn lighter_130_matches_qt() {
        // Golden values captured from real Qt 6.10 `QColor(c).lighter(130).name()`.
        let cases: &[(&str, &str)] = &[
            // Achromatic (the case actually reachable via the hover guard).
            ("#000000", "#000000"), // pure black stays black
            ("#1e1e1e", "#272727"),
            ("#202020", "#2a2a2a"),
            ("#212121", "#2b2b2b"),
            ("#ffffff", "#ffffff"),
            // Chromatic, no value overflow.
            ("#3a2a3a", "#4b374b"),
            ("#102030", "#152a3e"),
            ("#7f7e74", "#a5a497"),
            ("#123456", "#174470"),
            // Chromatic with value overflow (saturation is shed).
            ("#ff0000", "#ff4c4c"),
            ("#00ff00", "#4cff4c"),
            ("#0000ff", "#4c4cff"),
            ("#569cd6", "#7ec4ff"),
            ("#abcdef", "#eef7ff"),
            ("#fe0102", "#ff4c4d"),
        ];
        for (input, expected) in cases {
            let c = Color::parse(input).unwrap();
            assert_eq!(c.lighter_130().to_hex(), *expected, "lighter_130({input})");
        }
    }

    #[test]
    fn lerp_rgb_truncates_and_clamps() {
        // Matches C++ `a + int((b-a)*f)` then qBound(0,..,255).
        let a = Color::rgb(0x7f, 0x7e, 0x74); // 127,126,116  (Long Night textDim)
        let b = Color::rgb(220, 180, 120);
        let got = lerp_rgb(a, b, 0.35);
        // r: 127 + int(93*0.35)=127+int(32.55)=127+32=159
        // g: 126 + int(54*0.35)=126+int(18.9)=126+18=144
        // b: 116 + int(4*0.35)=116+int(1.4)=116+1=117
        assert_eq!(got, Color::rgb(159, 144, 117));
    }
}
