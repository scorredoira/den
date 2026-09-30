use alacritty_terminal::{
    term::color::Colors,
    vte::ansi::{Color, NamedColor, Rgb},
};
use gpui_kit::{Hsla, Rgba, rgb};

/// Terminal colors: the 16 ANSI ones for the light or dark theme, and the
/// text and background from the app theme.
pub struct Palette {
    ansi: [Hsla; 16],
    pub foreground: Hsla,
    pub background: Hsla,
    pub cursor: Hsla,
    pub selection: Hsla,
}

// One Dark and One Light tones, readable on their backgrounds.
const DARK: [u32; 16] = [
    0x3f4451, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xd7dae0, //
    0x5c6370, 0xef7b85, 0xa5d086, 0xf0cb8a, 0x74bcf7, 0xd28ae6, 0x66c5d1, 0xffffff,
];
const LIGHT: [u32; 16] = [
    0x383a42, 0xca1243, 0x50a14f, 0x986801, 0x4078f2, 0xa626a4, 0x0184bc, 0xa0a1a7, //
    0x4f525e, 0xe45649, 0x3e953a, 0xb58900, 0x2d64e0, 0x9a2ea0, 0x0997b3, 0xfafafa,
];

impl Palette {
    pub fn new(dark: bool, foreground: Hsla, background: Hsla, selection: Hsla) -> Self {
        let table = if dark { DARK } else { LIGHT };
        Self {
            ansi: table.map(|hex| rgb(hex).into()),
            foreground,
            background,
            cursor: foreground,
            selection,
        }
    }

    /// Resolves a cell color. `overrides` are the ones the application
    /// changed with OSC sequences.
    pub fn resolve(&self, color: &Color, overrides: &Colors) -> Hsla {
        match color {
            Color::Spec(rgb) => rgb_to_hsla(*rgb),
            Color::Indexed(ix) => self.indexed(*ix as usize, overrides),
            Color::Named(named) => self.named(*named, overrides),
        }
    }

    fn named(&self, named: NamedColor, overrides: &Colors) -> Hsla {
        if let Some(rgb) = overrides[named] {
            return rgb_to_hsla(rgb);
        }
        match named {
            NamedColor::Foreground | NamedColor::BrightForeground => self.foreground,
            NamedColor::DimForeground => self.foreground.opacity(0.7),
            NamedColor::Background => self.background,
            NamedColor::Cursor => self.cursor,
            NamedColor::DimBlack
            | NamedColor::DimRed
            | NamedColor::DimGreen
            | NamedColor::DimYellow
            | NamedColor::DimBlue
            | NamedColor::DimMagenta
            | NamedColor::DimCyan
            | NamedColor::DimWhite => {
                let base = named as usize - NamedColor::DimBlack as usize;
                self.ansi[base].opacity(0.7)
            }
            _ => self.ansi[named as usize],
        }
    }

    fn indexed(&self, ix: usize, overrides: &Colors) -> Hsla {
        if let Some(rgb) = overrides[ix] {
            return rgb_to_hsla(rgb);
        }
        match ix {
            0..=15 => self.ansi[ix],
            // 6x6x6 cube with xterm's steps.
            16..=231 => {
                let ix = ix - 16;
                let step = |v: usize| if v == 0 { 0 } else { (v * 40 + 55) as u8 };
                rgb_to_hsla(Rgb {
                    r: step(ix / 36),
                    g: step((ix / 6) % 6),
                    b: step(ix % 6),
                })
            }
            // Grayscale.
            _ => {
                let v = ((ix - 232) * 10 + 8) as u8;
                rgb_to_hsla(Rgb { r: v, g: v, b: v })
            }
        }
    }

    /// The RGB value of an indexed color, to answer OSC 4/10/11.
    pub fn rgb_at(&self, ix: usize, overrides: &Colors) -> Rgb {
        let hsla = match ix {
            0..=255 => self.indexed(ix, overrides),
            ix if ix == NamedColor::Foreground as usize => self.foreground,
            ix if ix == NamedColor::Background as usize => self.background,
            _ => self.cursor,
        };
        let rgba = Rgba::from(hsla);
        Rgb {
            r: (rgba.r * 255.) as u8,
            g: (rgba.g * 255.) as u8,
            b: (rgba.b * 255.) as u8,
        }
    }
}

fn rgb_to_hsla(rgb: Rgb) -> Hsla {
    Rgba {
        r: rgb.r as f32 / 255.,
        g: rgb.g as f32 / 255.,
        b: rgb.b as f32 / 255.,
        a: 1.,
    }
    .into()
}
