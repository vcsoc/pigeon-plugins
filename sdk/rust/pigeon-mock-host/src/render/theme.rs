//! Theme tokens (protocol section 12) resolved to RGBA for the software renderer.

use std::collections::BTreeMap;

use pigeon_plugin_sdk::protocol::Theme as ProtocolTheme;

/// A resolved color (straight alpha, 8 bits per channel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba(pub [u8; 4]);

impl Rgba {
    /// Parse `#rrggbb` or `#rrggbbaa`.
    pub fn parse_hex(text: &str) -> Option<Rgba> {
        let hex = text.strip_prefix('#')?;
        let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
        match hex.len() {
            6 => Some(Rgba([byte(0)?, byte(2)?, byte(4)?, 255])),
            8 => Some(Rgba([byte(0)?, byte(2)?, byte(4)?, byte(6)?])),
            _ => None,
        }
    }
}

/// The renderer's palette. Starts from Pigeon's dark palette and is overridden by the host theme.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub colors: BTreeMap<String, Rgba>,
    pub radius: f32,
    pub spacing: f32,
    pub scale: f32,
    pub dark: bool,
}

const DARK: &[(&str, &str)] = &[
    ("bg", "#1b1d22"),
    ("bg_panel", "#23262d"),
    ("bg_raised", "#2b2f38"),
    ("bg_input", "#16181c"),
    ("fg", "#e6e8ee"),
    ("fg_weak", "#9aa3b2"),
    ("accent", "#3b82f6"),
    ("accent_fg", "#ffffff"),
    ("danger", "#ef4444"),
    ("warning", "#f59e0b"),
    ("success", "#22c55e"),
    ("border", "#343945"),
    ("hover", "#30353f"),
    ("selection", "#2d4f86"),
    ("focus_ring", "#60a5fa"),
];

const LIGHT: &[(&str, &str)] = &[
    ("bg", "#f5f6f8"),
    ("bg_panel", "#ffffff"),
    ("bg_raised", "#ffffff"),
    ("bg_input", "#ffffff"),
    ("fg", "#1d2026"),
    ("fg_weak", "#5b6472"),
    ("accent", "#2563eb"),
    ("accent_fg", "#ffffff"),
    ("danger", "#dc2626"),
    ("warning", "#b45309"),
    ("success", "#15803d"),
    ("border", "#d5d9e0"),
    ("hover", "#e9ecf1"),
    ("selection", "#bfd4ff"),
    ("focus_ring", "#2563eb"),
];

impl Theme {
    /// The built-in dark palette.
    pub fn dark() -> Theme {
        Theme::from_table(DARK, true, 4.0, 4.0, 2.0)
    }

    /// The built-in light palette.
    pub fn light() -> Theme {
        Theme::from_table(LIGHT, false, 4.0, 4.0, 2.0)
    }

    fn from_table(table: &[(&str, &str)], dark: bool, radius: f32, spacing: f32, scale: f32) -> Theme {
        let colors = table.iter().filter_map(|(k, v)| Rgba::parse_hex(v).map(|c| (k.to_string(), c))).collect();
        Theme { colors, radius, spacing, scale, dark }
    }

    /// Apply the host's theme message: its colors override the palette; mode picks the base.
    pub fn from_protocol(theme: &ProtocolTheme) -> Theme {
        let mut base = if theme.mode == "light" { Theme::light() } else { Theme::dark() };
        for (k, v) in &theme.colors {
            if let Some(c) = Rgba::parse_hex(v) {
                base.colors.insert(k.clone(), c);
            }
        }
        if theme.radius > 0.0 {
            base.radius = theme.radius;
        }
        if theme.spacing > 0.0 {
            base.spacing = theme.spacing;
        }
        base
    }

    /// Resolve a color value: a token name, or `#rrggbb[aa]`. Unknown tokens resolve to magenta so
    /// that a missing token is visible in golden images.
    pub fn resolve(&self, value: &str) -> Rgba {
        if let Some(c) = Rgba::parse_hex(value) {
            return c;
        }
        self.colors.get(value).copied().unwrap_or(Rgba([255, 0, 255, 255]))
    }

    /// Token lookup with a fallback.
    pub fn token(&self, name: &str, fallback: &str) -> Rgba {
        self.colors.get(name).copied().unwrap_or_else(|| Rgba::parse_hex(fallback).unwrap_or(Rgba([255, 0, 255, 255])))
    }
}

/// The dark theme as a protocol `hello` theme (what the mock sends by default).
pub fn dark_theme_protocol() -> ProtocolTheme { protocol_theme(false, 2.0) }

/// Built-in mock palette, serialized for `hello`.
pub fn protocol_theme(light: bool, scale: f32) -> ProtocolTheme {
    ProtocolTheme {
        mode: if light { "light" } else { "dark" }.into(),
        scale,
        font: pigeon_plugin_sdk::protocol::Fonts { ui: "Ubuntu".into(), mono: "Hack".into() },
        colors: (if light { LIGHT } else { DARK }).iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        radius: 4.0,
        spacing: 4.0,
    }
}

pub fn parse_color_hex_or_token(text: &str, theme: &Theme) -> Rgba {
    theme.resolve(text)
}
