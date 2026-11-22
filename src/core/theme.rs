//! Colour themes: the built-ins and the user's own, as one editable data type.
//!
//! A [`ThemeSpec`] names every colour the UI and the terminal use. The built-ins (matcha,
//! light, and the extra dark and light presets) are specs too, so a custom theme can start
//! from any of them. Specs live in the config (`custom_themes`) and travel as small JSON
//! files (`*.ruxt-theme.json`) whose colours are `#rrggbb` strings, so they can be edited
//! by hand as well.

use alacritty_terminal::vte::ansi::Rgb;
use anyhow::{bail, Context, Result};
use egui::Color32;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Custom themes kept at most.
pub const MAX_CUSTOM_THEMES: usize = 32;
/// Longest theme name (characters).
pub const MAX_NAME_LEN: usize = 40;
/// File name suffix used for exported themes.
pub const FILE_SUFFIX: &str = ".ruxt-theme.json";

/// An sRGB colour serialised as `#rrggbb`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb8(pub [u8; 3]);

impl Rgb8 {
    pub const fn hex(v: u32) -> Self {
        Self([((v >> 16) & 0xff) as u8, ((v >> 8) & 0xff) as u8, (v & 0xff) as u8])
    }
    pub fn color32(self) -> Color32 {
        Color32::from_rgb(self.0[0], self.0[1], self.0[2])
    }
    pub fn rgb(self) -> Rgb {
        Rgb {
            r: self.0[0],
            g: self.0[1],
            b: self.0[2],
        }
    }
    /// Blend towards black (`f < 0`) or white (`f > 0`), `|f| <= 1`.
    pub fn shade(self, f: f32) -> Self {
        let f = f.clamp(-1.0, 1.0);
        let m = |c: u8| -> u8 {
            let c = c as f32;
            let v = if f < 0.0 { c * (1.0 + f) } else { c + (255.0 - c) * f };
            v.round().clamp(0.0, 255.0) as u8
        };
        Self([m(self.0[0]), m(self.0[1]), m(self.0[2])])
    }
}

impl std::fmt::Display for Rgb8 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.0[0], self.0[1], self.0[2])
    }
}

impl std::str::FromStr for Rgb8 {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        let h = s.trim().trim_start_matches('#');
        if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("colour {s:?} is not #rrggbb");
        }
        let v = u32::from_str_radix(h, 16)?;
        Ok(Self::hex(v))
    }
}

impl Serialize for Rgb8 {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Rgb8 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Every colour of a theme. Unknown fields in a file are ignored; missing ones take the
/// matcha defaults, so a hand-written file only needs the colours it wants to change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeSpec {
    pub name: String,
    /// Base egui visuals (dark or light): affects shadows, default strokes and such.
    pub dark: bool,
    // ---- UI ----
    pub panel: Rgb8,
    pub window: Rgb8,
    /// Text fields and other "sunken" surfaces.
    pub field: Rgb8,
    pub faint: Rgb8,
    pub widget: Rgb8,
    pub widget_hover: Rgb8,
    pub widget_active: Rgb8,
    pub text: Rgb8,
    pub weak_text: Rgb8,
    pub accent: Rgb8,
    pub selection: Rgb8,
    pub hyperlink: Rgb8,
    pub warn: Rgb8,
    pub error: Rgb8,
    pub stroke: Rgb8,
    // ---- terminal ----
    pub term_fg: Rgb8,
    pub term_bg: Rgb8,
    pub term_cursor: Rgb8,
    pub ansi: [Rgb8; 16],
}

impl Default for ThemeSpec {
    fn default() -> Self {
        Self::matcha()
    }
}

impl ThemeSpec {
    /// Dark "matcha": deep green-black surfaces with matcha-green accents.
    pub fn matcha() -> Self {
        Self {
            name: "Matcha".into(),
            dark: true,
            panel: Rgb8::hex(0x202a1e),
            window: Rgb8::hex(0x1b231a),
            field: Rgb8::hex(0x141a13),
            faint: Rgb8::hex(0x273225),
            widget: Rgb8::hex(0x2e3c2b),
            widget_hover: Rgb8::hex(0x3c4f38),
            widget_active: Rgb8::hex(0x5a7f45),
            text: Rgb8::hex(0xd9e3cf),
            weak_text: Rgb8::hex(0xa9b69f),
            accent: Rgb8::hex(0xa8c66c),
            selection: Rgb8::hex(0x4f723a),
            hyperlink: Rgb8::hex(0xa8c66c),
            warn: Rgb8::hex(0xe0b35c),
            error: Rgb8::hex(0xe67a7a),
            stroke: Rgb8::hex(0x3a4a36),
            term_fg: Rgb8::hex(0xd9e3cf),
            term_bg: Rgb8::hex(0x1a2218),
            term_cursor: Rgb8::hex(0xa8c66c),
            ansi: [
                Rgb8::hex(0x2b3528),
                Rgb8::hex(0xd7696a),
                Rgb8::hex(0x8fbf6a),
                Rgb8::hex(0xd9c468),
                Rgb8::hex(0x6fa7c7),
                Rgb8::hex(0xb48ab8),
                Rgb8::hex(0x6fbfb0),
                Rgb8::hex(0xc5d1bb),
                Rgb8::hex(0x5d6b58),
                Rgb8::hex(0xe68a8b),
                Rgb8::hex(0xa9d77f),
                Rgb8::hex(0xebd87f),
                Rgb8::hex(0x8cc1dd),
                Rgb8::hex(0xc9a3cd),
                Rgb8::hex(0x8fd4c6),
                Rgb8::hex(0xe8efe0),
            ],
        }
    }

    /// Light: warm paper with a green accent and high-contrast ANSI colours.
    pub fn light() -> Self {
        Self {
            name: "Light".into(),
            dark: false,
            panel: Rgb8::hex(0xf4f4ee),
            window: Rgb8::hex(0xfbfbf6),
            field: Rgb8::hex(0xffffff),
            faint: Rgb8::hex(0xecece4),
            widget: Rgb8::hex(0xe6e6de),
            widget_hover: Rgb8::hex(0xdcdcd2),
            widget_active: Rgb8::hex(0xc8e4b4),
            text: Rgb8::hex(0x23281f),
            weak_text: Rgb8::hex(0x6b6f66),
            accent: Rgb8::hex(0x3f7f2f),
            selection: Rgb8::hex(0xc8e4b4),
            hyperlink: Rgb8::hex(0x3f7f2f),
            warn: Rgb8::hex(0xa8700c),
            error: Rgb8::hex(0xc0392b),
            stroke: Rgb8::hex(0xd0d0c6),
            term_fg: Rgb8::hex(0x23281f),
            term_bg: Rgb8::hex(0xfbfbf6),
            term_cursor: Rgb8::hex(0x3f7f2f),
            ansi: light_ansi(),
        }
    }

    /// Dark purple surfaces with a green accent.
    pub fn purple_green() -> Self {
        Self {
            name: "Purple Green".into(),
            dark: true,
            panel: Rgb8::hex(0x22182c),
            window: Rgb8::hex(0x1b1424),
            field: Rgb8::hex(0x140f1c),
            faint: Rgb8::hex(0x2c2238),
            widget: Rgb8::hex(0x382a4a),
            widget_hover: Rgb8::hex(0x4a3860),
            widget_active: Rgb8::hex(0x3d6b48),
            text: Rgb8::hex(0xeee8f4),
            weak_text: Rgb8::hex(0xb7adc4),
            accent: Rgb8::hex(0x7dce78),
            selection: Rgb8::hex(0x2f5a40),
            hyperlink: Rgb8::hex(0xc4a6f0),
            warn: Rgb8::hex(0xe0b35c),
            error: Rgb8::hex(0xe67a8a),
            stroke: Rgb8::hex(0x4a3a5c),
            term_fg: Rgb8::hex(0xe7f2df),
            term_bg: Rgb8::hex(0x16111e),
            term_cursor: Rgb8::hex(0x8ed888),
            ansi: [
                Rgb8::hex(0x2a2236),
                Rgb8::hex(0xe07080),
                Rgb8::hex(0x7dce78),
                Rgb8::hex(0xe0c46a),
                Rgb8::hex(0x8aa4e8),
                Rgb8::hex(0xc49be0),
                Rgb8::hex(0x6ec8b0),
                Rgb8::hex(0xd8d0e4),
                Rgb8::hex(0x5a4e6c),
                Rgb8::hex(0xf0909c),
                Rgb8::hex(0xa4e89a),
                Rgb8::hex(0xf0dc88),
                Rgb8::hex(0xa8c0f4),
                Rgb8::hex(0xdcb4f0),
                Rgb8::hex(0x8ee0c8),
                Rgb8::hex(0xf6f2fa),
            ],
        }
    }

    /// Dark blue-slate, like a harbour at night.
    pub fn harbor() -> Self {
        Self {
            name: "Harbor".into(),
            dark: true,
            panel: Rgb8::hex(0x1c2832),
            window: Rgb8::hex(0x162028),
            field: Rgb8::hex(0x10181e),
            faint: Rgb8::hex(0x243440),
            widget: Rgb8::hex(0x2c4050),
            widget_hover: Rgb8::hex(0x3a5468),
            widget_active: Rgb8::hex(0x2f6d8c),
            text: Rgb8::hex(0xe4eef4),
            weak_text: Rgb8::hex(0x9aafbc),
            accent: Rgb8::hex(0x6ec4e0),
            selection: Rgb8::hex(0x1e4e66),
            hyperlink: Rgb8::hex(0x8ad4ee),
            warn: Rgb8::hex(0xe0b35c),
            error: Rgb8::hex(0xe07a7a),
            stroke: Rgb8::hex(0x3a4e5c),
            term_fg: Rgb8::hex(0xdceaf2),
            term_bg: Rgb8::hex(0x121a20),
            term_cursor: Rgb8::hex(0x7ed0ea),
            ansi: [
                Rgb8::hex(0x243038),
                Rgb8::hex(0xd7696a),
                Rgb8::hex(0x6fbf8a),
                Rgb8::hex(0xd9c468),
                Rgb8::hex(0x6eb0d4),
                Rgb8::hex(0xa888c8),
                Rgb8::hex(0x62c4c4),
                Rgb8::hex(0xc5d4de),
                Rgb8::hex(0x5a6e7c),
                Rgb8::hex(0xe68a8b),
                Rgb8::hex(0x8ed9a4),
                Rgb8::hex(0xebd87f),
                Rgb8::hex(0x8ec8ea),
                Rgb8::hex(0xc4a6e0),
                Rgb8::hex(0x86e0e0),
                Rgb8::hex(0xeef4f8),
            ],
        }
    }

    /// Dark warm brown with an amber accent.
    pub fn ember() -> Self {
        Self {
            name: "Ember".into(),
            dark: true,
            panel: Rgb8::hex(0x2a221c),
            window: Rgb8::hex(0x221c16),
            field: Rgb8::hex(0x18140f),
            faint: Rgb8::hex(0x362c24),
            widget: Rgb8::hex(0x403228),
            widget_hover: Rgb8::hex(0x564434),
            widget_active: Rgb8::hex(0x8a5a28),
            text: Rgb8::hex(0xf6eee4),
            weak_text: Rgb8::hex(0xc2b09a),
            accent: Rgb8::hex(0xe0a04a),
            selection: Rgb8::hex(0x6a4424),
            hyperlink: Rgb8::hex(0xf0b86a),
            warn: Rgb8::hex(0xe0c070),
            error: Rgb8::hex(0xe07060),
            stroke: Rgb8::hex(0x524436),
            term_fg: Rgb8::hex(0xf4ece2),
            term_bg: Rgb8::hex(0x1a1510),
            term_cursor: Rgb8::hex(0xf0b060),
            ansi: [
                Rgb8::hex(0x322820),
                Rgb8::hex(0xd7695a),
                Rgb8::hex(0x8fbf6a),
                Rgb8::hex(0xe0b050),
                Rgb8::hex(0x7aa0c0),
                Rgb8::hex(0xc09080),
                Rgb8::hex(0x70b0a0),
                Rgb8::hex(0xe0d4c4),
                Rgb8::hex(0x6a5c50),
                Rgb8::hex(0xee8878),
                Rgb8::hex(0xa9d77f),
                Rgb8::hex(0xf0c868),
                Rgb8::hex(0x98c0dc),
                Rgb8::hex(0xe0b0a0),
                Rgb8::hex(0x90d0c0),
                Rgb8::hex(0xf8f2ea),
            ],
        }
    }

    /// Neutral charcoal with a cool blue accent.
    pub fn ink() -> Self {
        Self {
            name: "Ink".into(),
            dark: true,
            panel: Rgb8::hex(0x1e1e24),
            window: Rgb8::hex(0x18181c),
            field: Rgb8::hex(0x101014),
            faint: Rgb8::hex(0x282830),
            widget: Rgb8::hex(0x32323c),
            widget_hover: Rgb8::hex(0x444450),
            widget_active: Rgb8::hex(0x3a5088),
            text: Rgb8::hex(0xececf2),
            weak_text: Rgb8::hex(0xa6a6b4),
            accent: Rgb8::hex(0x8aa8f0),
            selection: Rgb8::hex(0x2c3e6e),
            hyperlink: Rgb8::hex(0xa8c0f8),
            warn: Rgb8::hex(0xe0b35c),
            error: Rgb8::hex(0xe07a7a),
            stroke: Rgb8::hex(0x3c3c48),
            term_fg: Rgb8::hex(0xe8e8f0),
            term_bg: Rgb8::hex(0x141418),
            term_cursor: Rgb8::hex(0x9ab4f4),
            ansi: [
                Rgb8::hex(0x2a2a32),
                Rgb8::hex(0xd7696a),
                Rgb8::hex(0x7ec87a),
                Rgb8::hex(0xd4c46a),
                Rgb8::hex(0x7aa2e8),
                Rgb8::hex(0xb48ad0),
                Rgb8::hex(0x6ec0c0),
                Rgb8::hex(0xd0d0d8),
                Rgb8::hex(0x5c5c68),
                Rgb8::hex(0xee8a8a),
                Rgb8::hex(0x9ee09a),
                Rgb8::hex(0xeee080),
                Rgb8::hex(0xa4c0f4),
                Rgb8::hex(0xd0a8e8),
                Rgb8::hex(0x90e0e0),
                Rgb8::hex(0xf4f4f8),
            ],
        }
    }

    /// Deep teal forest, cooler than matcha.
    pub fn pine() -> Self {
        Self {
            name: "Pine".into(),
            dark: true,
            panel: Rgb8::hex(0x142422),
            window: Rgb8::hex(0x101e1c),
            field: Rgb8::hex(0x0c1614),
            faint: Rgb8::hex(0x1c322e),
            widget: Rgb8::hex(0x24423c),
            widget_hover: Rgb8::hex(0x30584e),
            widget_active: Rgb8::hex(0x1a6e5c),
            text: Rgb8::hex(0xdff4ee),
            weak_text: Rgb8::hex(0x96b8ae),
            accent: Rgb8::hex(0x3ec8a8),
            selection: Rgb8::hex(0x14584c),
            hyperlink: Rgb8::hex(0x5ed8bc),
            warn: Rgb8::hex(0xe0b35c),
            error: Rgb8::hex(0xe07a7a),
            stroke: Rgb8::hex(0x2c4a44),
            term_fg: Rgb8::hex(0xd8f0e8),
            term_bg: Rgb8::hex(0x0e1a18),
            term_cursor: Rgb8::hex(0x4ed4b4),
            ansi: [
                Rgb8::hex(0x1c302c),
                Rgb8::hex(0xd7696a),
                Rgb8::hex(0x5ec898),
                Rgb8::hex(0xd4c46a),
                Rgb8::hex(0x6aa8c8),
                Rgb8::hex(0xa888c0),
                Rgb8::hex(0x40c8b8),
                Rgb8::hex(0xc8e0d8),
                Rgb8::hex(0x4e6e66),
                Rgb8::hex(0xee8a8a),
                Rgb8::hex(0x80e0b0),
                Rgb8::hex(0xeee080),
                Rgb8::hex(0x8ec4e0),
                Rgb8::hex(0xc8a8dc),
                Rgb8::hex(0x70e8d8),
                Rgb8::hex(0xf0f8f4),
            ],
        }
    }

    /// Cool white paper with a blue accent.
    pub fn porcelain() -> Self {
        Self {
            name: "Porcelain".into(),
            dark: false,
            panel: Rgb8::hex(0xf2f4f6),
            window: Rgb8::hex(0xfbfcfd),
            field: Rgb8::hex(0xffffff),
            faint: Rgb8::hex(0xe6eaee),
            widget: Rgb8::hex(0xdde3e8),
            widget_hover: Rgb8::hex(0xd0d8e0),
            widget_active: Rgb8::hex(0xc5daf0),
            text: Rgb8::hex(0x1c242c),
            weak_text: Rgb8::hex(0x5c6a76),
            accent: Rgb8::hex(0x1f6fbf),
            selection: Rgb8::hex(0xc5daf0),
            hyperlink: Rgb8::hex(0x1f6fbf),
            warn: Rgb8::hex(0xa8700c),
            error: Rgb8::hex(0xc0392b),
            stroke: Rgb8::hex(0xcdd6de),
            term_fg: Rgb8::hex(0x1c242c),
            term_bg: Rgb8::hex(0xfbfcfd),
            term_cursor: Rgb8::hex(0x1f6fbf),
            ansi: light_ansi(),
        }
    }

    /// Warm cream paper with an amber accent.
    pub fn sand() -> Self {
        Self {
            name: "Sand".into(),
            dark: false,
            panel: Rgb8::hex(0xf6f1e6),
            window: Rgb8::hex(0xfbf8f1),
            field: Rgb8::hex(0xfffdf8),
            faint: Rgb8::hex(0xeee6d6),
            widget: Rgb8::hex(0xe6dcc8),
            widget_hover: Rgb8::hex(0xdccfb6),
            widget_active: Rgb8::hex(0xf0d4a4),
            text: Rgb8::hex(0x2a2418),
            weak_text: Rgb8::hex(0x6e6456),
            accent: Rgb8::hex(0xa86a10),
            selection: Rgb8::hex(0xf0d4a4),
            hyperlink: Rgb8::hex(0x8a5a10),
            warn: Rgb8::hex(0xa8700c),
            error: Rgb8::hex(0xc0392b),
            stroke: Rgb8::hex(0xddd2bc),
            term_fg: Rgb8::hex(0x2a2418),
            term_bg: Rgb8::hex(0xfbf8f1),
            term_cursor: Rgb8::hex(0xa86a10),
            ansi: light_ansi(),
        }
    }

    /// Pale mint paper with a deep green accent.
    pub fn mint() -> Self {
        Self {
            name: "Mint".into(),
            dark: false,
            panel: Rgb8::hex(0xeef6f2),
            window: Rgb8::hex(0xf7fbf9),
            field: Rgb8::hex(0xffffff),
            faint: Rgb8::hex(0xe0eee8),
            widget: Rgb8::hex(0xd4e6de),
            widget_hover: Rgb8::hex(0xc4ddd4),
            widget_active: Rgb8::hex(0xb6e4cc),
            text: Rgb8::hex(0x1a2822),
            weak_text: Rgb8::hex(0x5a7068),
            accent: Rgb8::hex(0x1f7a58),
            selection: Rgb8::hex(0xb6e4cc),
            hyperlink: Rgb8::hex(0x1f7a58),
            warn: Rgb8::hex(0xa8700c),
            error: Rgb8::hex(0xc0392b),
            stroke: Rgb8::hex(0xc6ddd4),
            term_fg: Rgb8::hex(0x1a2822),
            term_bg: Rgb8::hex(0xf7fbf9),
            term_cursor: Rgb8::hex(0x1f7a58),
            ansi: light_ansi(),
        }
    }

    /// Blush paper with a rose accent.
    pub fn rose() -> Self {
        Self {
            name: "Rose".into(),
            dark: false,
            panel: Rgb8::hex(0xf8f0f2),
            window: Rgb8::hex(0xfdf8f9),
            field: Rgb8::hex(0xffffff),
            faint: Rgb8::hex(0xf0e4e8),
            widget: Rgb8::hex(0xe8d6dc),
            widget_hover: Rgb8::hex(0xdec8d0),
            widget_active: Rgb8::hex(0xf0c4d0),
            text: Rgb8::hex(0x2c1c22),
            weak_text: Rgb8::hex(0x745860),
            accent: Rgb8::hex(0xb04068),
            selection: Rgb8::hex(0xf0c4d0),
            hyperlink: Rgb8::hex(0xa03860),
            warn: Rgb8::hex(0xa8700c),
            error: Rgb8::hex(0xc0392b),
            stroke: Rgb8::hex(0xe4d0d6),
            term_fg: Rgb8::hex(0x2c1c22),
            term_bg: Rgb8::hex(0xfdf8f9),
            term_cursor: Rgb8::hex(0xb04068),
            ansi: light_ansi(),
        }
    }

    /// Pale lavender paper with a purple accent.
    pub fn lavender() -> Self {
        Self {
            name: "Lavender".into(),
            dark: false,
            panel: Rgb8::hex(0xf4f0f8),
            window: Rgb8::hex(0xfbf9fd),
            field: Rgb8::hex(0xffffff),
            faint: Rgb8::hex(0xeae4f2),
            widget: Rgb8::hex(0xe0d8ec),
            widget_hover: Rgb8::hex(0xd4c8e4),
            widget_active: Rgb8::hex(0xd4c4ee),
            text: Rgb8::hex(0x261c32),
            weak_text: Rgb8::hex(0x6a5e78),
            accent: Rgb8::hex(0x6a42a8),
            selection: Rgb8::hex(0xdcc8f0),
            hyperlink: Rgb8::hex(0x6a42a8),
            warn: Rgb8::hex(0xa8700c),
            error: Rgb8::hex(0xc0392b),
            stroke: Rgb8::hex(0xd6cce4),
            term_fg: Rgb8::hex(0x261c32),
            term_bg: Rgb8::hex(0xfbf9fd),
            term_cursor: Rgb8::hex(0x6a42a8),
            ansi: light_ansi(),
        }
    }

    /// Every built-in, dark presets first. Used by the contrast test.
    #[cfg(test)]
    pub fn builtins() -> Vec<Self> {
        vec![
            Self::matcha(),
            Self::purple_green(),
            Self::harbor(),
            Self::ember(),
            Self::ink(),
            Self::pine(),
            Self::light(),
            Self::porcelain(),
            Self::sand(),
            Self::mint(),
            Self::rose(),
            Self::lavender(),
        ]
    }

    /// egui visuals built from the spec.
    pub fn visuals(&self) -> egui::Visuals {
        use egui::style::Selection;
        use egui::{Stroke, Visuals};
        let mut v = if self.dark { Visuals::dark() } else { Visuals::light() };
        let text = self.text.color32();
        let accent = self.accent.color32();
        v.override_text_color = Some(text);
        v.panel_fill = self.panel.color32();
        v.window_fill = self.window.color32();
        v.extreme_bg_color = self.field.color32();
        v.faint_bg_color = self.faint.color32();
        v.code_bg_color = self.field.color32();
        v.hyperlink_color = self.hyperlink.color32();
        v.warn_fg_color = self.warn.color32();
        v.error_fg_color = self.error.color32();
        v.window_stroke = Stroke::new(1.0, self.stroke.color32());
        v.selection = Selection {
            bg_fill: self.selection.color32(),
            stroke: Stroke::new(1.0, accent),
        };
        let strong = if self.dark {
            self.text.shade(0.5).color32()
        } else {
            self.text.shade(-0.5).color32()
        };
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = self.panel.color32();
        w.noninteractive.weak_bg_fill = self.panel.color32();
        w.noninteractive.bg_stroke = Stroke::new(1.0, self.stroke.color32());
        w.noninteractive.fg_stroke = Stroke::new(1.0, self.weak_text.color32());
        w.inactive.bg_fill = self.widget.color32();
        w.inactive.weak_bg_fill = self.widget.shade(if self.dark { -0.12 } else { 0.3 }).color32();
        w.inactive.bg_stroke = Stroke::new(1.0, self.stroke.color32());
        w.inactive.fg_stroke = Stroke::new(1.0, text);
        w.hovered.bg_fill = self.widget_hover.color32();
        w.hovered.weak_bg_fill = self.widget_hover.color32();
        w.hovered.bg_stroke = Stroke::new(1.0, accent);
        w.hovered.fg_stroke = Stroke::new(1.5, strong);
        w.active.bg_fill = self.widget_active.color32();
        w.active.weak_bg_fill = self.selection.color32();
        w.active.bg_stroke = Stroke::new(1.0, accent);
        w.active.fg_stroke = Stroke::new(2.0, strong);
        w.open.bg_fill = self.widget.shade(if self.dark { -0.12 } else { 0.3 }).color32();
        w.open.weak_bg_fill = w.open.bg_fill;
        w.open.bg_stroke = Stroke::new(1.0, self.stroke.color32());
        w.open.fg_stroke = Stroke::new(1.0, text);
        v
    }

    /// Terminal palette built from the spec.
    pub fn palette(&self) -> super::term::Palette {
        super::term::Palette {
            fg: self.term_fg.rgb(),
            bg: self.term_bg.rgb(),
            cursor: self.term_cursor.rgb(),
            ansi: self.ansi.map(Rgb8::rgb),
        }
    }

    /// Trim and bound the name; empty becomes "Custom".
    pub fn normalize_name(&mut self) {
        let clean = super::term::sanitize_title(self.name.trim());
        self.name = clean.chars().take(MAX_NAME_LEN).collect();
        if self.name.is_empty() {
            self.name = "Custom".into();
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Parse an exported theme (any extra keys are ignored).
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 64 * 1024 {
            bail!("theme file is too large");
        }
        let mut t: ThemeSpec = serde_json::from_slice(bytes).context("not a Ruxt theme file")?;
        t.normalize_name();
        Ok(t)
    }

    /// File name an export suggests.
    pub fn file_name(&self) -> String {
        let stem: String = self
            .name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        format!("{stem}{FILE_SUFFIX}")
    }
}

/// High-contrast ANSI colours shared by the light presets.
fn light_ansi() -> [Rgb8; 16] {
    [
        Rgb8::hex(0x000000),
        Rgb8::hex(0xc0392b),
        Rgb8::hex(0x2f7d32),
        Rgb8::hex(0x9a7d0a),
        Rgb8::hex(0x1f5fbf),
        Rgb8::hex(0x8e44ad),
        Rgb8::hex(0x138d75),
        Rgb8::hex(0x8c8c8c),
        Rgb8::hex(0x555555),
        Rgb8::hex(0xe74c3c),
        Rgb8::hex(0x43a047),
        Rgb8::hex(0xc9a800),
        Rgb8::hex(0x3f7fe0),
        Rgb8::hex(0xb57edc),
        Rgb8::hex(0x1abc9c),
        Rgb8::hex(0x303030),
    ]
}

/// Relative luminance, 0–1. Used by the contrast test.
#[cfg(test)]
fn luminance(c: Rgb8) -> f32 {
    let f = |v: u8| {
        let s = v as f32 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * f(c.0[0]) + 0.7152 * f(c.0[1]) + 0.0722 * f(c.0[2])
}

#[cfg(test)]
fn contrast(a: Rgb8, b: Rgb8) -> f32 {
    let (l1, l2) = (luminance(a), luminance(b));
    let (hi, lo) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
}

/// A name not used by any theme in `taken`: `name`, else `name (2)`, `name (3)`, …
pub fn unique_name(name: &str, taken: &[String]) -> String {
    if !taken.iter().any(|t| t == name) {
        return name.to_string();
    }
    for i in 2.. {
        let candidate = format!("{name} ({i})");
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_roundtrip_as_hex() {
        let c: Rgb8 = "#1A2218".parse().unwrap();
        assert_eq!(c, Rgb8::hex(0x1a2218));
        assert_eq!(c.to_string(), "#1a2218");
        assert!("#12345".parse::<Rgb8>().is_err());
        assert!("red".parse::<Rgb8>().is_err());
        assert_eq!(Rgb8::hex(0x808080).shade(-1.0), Rgb8::hex(0x000000));
        assert_eq!(Rgb8::hex(0x808080).shade(1.0), Rgb8::hex(0xffffff));
    }

    #[test]
    fn theme_json_roundtrip_and_partial_files() {
        let mut t = ThemeSpec::light();
        t.name = "  My Paper  ".into();
        t.normalize_name();
        assert_eq!(t.name, "My Paper");
        let json = t.to_json();
        assert!(json.contains("\"term_bg\": \"#fbfbf6\""), "{json}");
        let back = ThemeSpec::from_json(json.as_bytes()).unwrap();
        assert_eq!(back, t);
        // A hand-written file may list only a few colours.
        let partial =
            ThemeSpec::from_json(br##"{"name":"Night","dark":true,"accent":"#ff8800","unknown":1}"##).unwrap();
        assert_eq!(partial.accent, Rgb8::hex(0xff8800));
        assert_eq!(partial.panel, ThemeSpec::matcha().panel);
        assert_eq!(partial.name, "Night");
        let empty = ThemeSpec::from_json(br#"{"name":"\u001b[31m   "}"#).unwrap();
        assert_eq!(empty.name, "[31m");
        assert!(ThemeSpec::from_json(b"[1,2]").is_err());
        assert!(ThemeSpec::from_json(br##"{"accent":"blue"}"##).is_err());
        assert_eq!(t.file_name(), "My_Paper.ruxt-theme.json");
        let long = ThemeSpec::from_json(format!("{{\"name\":\"{}\"}}", "x".repeat(100)).as_bytes()).unwrap();
        assert_eq!(long.name.chars().count(), MAX_NAME_LEN);
    }

    #[test]
    fn unique_names_get_a_counter() {
        let taken = vec!["A".to_string(), "A (2)".to_string()];
        assert_eq!(unique_name("B", &taken), "B");
        assert_eq!(unique_name("A", &taken), "A (3)");
    }

    #[test]
    fn built_ins_build_visuals_and_palettes() {
        let all = ThemeSpec::builtins();
        assert_eq!(all.iter().filter(|t| t.dark).count(), 6);
        assert_eq!(all.iter().filter(|t| !t.dark).count(), 6);
        let mut names = all.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), all.len());
        for t in &all {
            let v = t.visuals();
            assert_eq!(v.panel_fill, t.panel.color32());
            assert_eq!(v.dark_mode, t.dark);
            let p = t.palette();
            assert_eq!(p.bg, t.term_bg.rgb());
            assert_eq!(p.ansi[1], t.ansi[1].rgb());
            assert!(contrast(t.text, t.panel) >= 4.5, "{} text/panel contrast", t.name);
            assert!(contrast(t.term_fg, t.term_bg) >= 4.5, "{} terminal contrast", t.name);
        }
        let pg = ThemeSpec::purple_green();
        assert!(pg.dark);
        assert!(pg.accent.0[1] > pg.accent.0[0] && pg.accent.0[1] > pg.accent.0[2]);
        assert!(pg.panel.0[2] > pg.panel.0[1] && pg.panel.0[0] > pg.panel.0[1]);
        assert_eq!(ThemeSpec::light().ansi, light_ansi());
    }
}
