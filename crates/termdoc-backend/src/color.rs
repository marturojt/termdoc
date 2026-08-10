//! Color degradation: truecolor → 256 → 16 → none.
//!
//! This is the COLOR rung of the ladder in docs/DESIGN.md §5. The layout always asks for
//! the color it would like; here it is translated into what the terminal actually
//! supports.

use termdoc_core::{Color, NamedColor};
use termdoc_term::ColorDepth;

/// The SGR code for a foreground color, or `None` when nothing should be emitted.
pub fn fg_code(color: Color, depth: ColorDepth) -> Option<String> {
    sgr_code(color, depth, false)
}

pub fn bg_code(color: Color, depth: ColorDepth) -> Option<String> {
    sgr_code(color, depth, true)
}

fn sgr_code(color: Color, depth: ColorDepth, background: bool) -> Option<String> {
    match depth {
        ColorDepth::None => None,
        ColorDepth::TrueColor => Some(match color {
            Color::Rgb(r, g, b) => {
                let base = if background { 48 } else { 38 };
                format!("{base};2;{r};{g};{b}")
            }
            Color::Indexed(n) => {
                let base = if background { 48 } else { 38 };
                format!("{base};5;{n}")
            }
            Color::Named(n) => named_code(n, background).to_string(),
        }),
        ColorDepth::Ansi256 => Some(match color {
            Color::Rgb(r, g, b) => {
                let base = if background { 48 } else { 38 };
                format!("{base};5;{}", rgb_to_256(r, g, b))
            }
            Color::Indexed(n) => {
                let base = if background { 48 } else { 38 };
                format!("{base};5;{n}")
            }
            Color::Named(n) => named_code(n, background).to_string(),
        }),
        ColorDepth::Ansi16 => {
            let named = match color {
                Color::Named(n) => n,
                Color::Rgb(r, g, b) => rgb_to_named(r, g, b),
                Color::Indexed(n) => indexed_to_named(n),
            };
            Some(named_code(named, background).to_string())
        }
    }
}

fn named_code(color: NamedColor, background: bool) -> u16 {
    use NamedColor::*;
    let (normal, bright) = match color {
        Black => (30, 90),
        Red => (31, 91),
        Green => (32, 92),
        Yellow => (33, 93),
        Blue => (34, 94),
        Magenta => (35, 95),
        Cyan => (36, 96),
        White => (37, 97),
        BrightBlack => (90, 90),
        BrightRed => (91, 91),
        BrightGreen => (92, 92),
        BrightYellow => (93, 93),
        BrightBlue => (94, 94),
        BrightMagenta => (95, 95),
        BrightCyan => (96, 96),
        BrightWhite => (97, 97),
    };
    let code = if matches!(
        color,
        BrightBlack
            | BrightRed
            | BrightGreen
            | BrightYellow
            | BrightBlue
            | BrightMagenta
            | BrightCyan
            | BrightWhite
    ) {
        bright
    } else {
        normal
    };
    if background { code + 10 } else { code }
}

/// RGB to a 256-palette index: the 6x6x6 cube (16..232) or the grayscale ramp.
pub fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    // Grays have their own ramp with far more resolution than the cube, so it pays to
    // detect them before quantizing.
    if r == g && g == b {
        if r < 8 {
            return 16;
        }
        if r > 248 {
            return 231;
        }
        return 232 + ((r as u16 - 8) * 24 / 247) as u8;
    }
    let q = |v: u8| -> u16 { (v as u16 * 5 + 127) / 255 };
    (16 + 36 * q(r) + 6 * q(g) + q(b)) as u8
}

/// RGB to the nearest of the 16 ANSI names, by Euclidean distance.
pub fn rgb_to_named(r: u8, g: u8, b: u8) -> NamedColor {
    use NamedColor::*;
    // The nominal xterm palette values.
    const TABLE: [(NamedColor, (u8, u8, u8)); 16] = [
        (Black, (0, 0, 0)),
        (Red, (128, 0, 0)),
        (Green, (0, 128, 0)),
        (Yellow, (128, 128, 0)),
        (Blue, (0, 0, 128)),
        (Magenta, (128, 0, 128)),
        (Cyan, (0, 128, 128)),
        (White, (192, 192, 192)),
        (BrightBlack, (128, 128, 128)),
        (BrightRed, (255, 0, 0)),
        (BrightGreen, (0, 255, 0)),
        (BrightYellow, (255, 255, 0)),
        (BrightBlue, (0, 0, 255)),
        (BrightMagenta, (255, 0, 255)),
        (BrightCyan, (0, 255, 255)),
        (BrightWhite, (255, 255, 255)),
    ];

    let mut best = Black;
    let mut best_dist = u32::MAX;
    for (name, (tr, tg, tb)) in TABLE {
        let dr = r.abs_diff(tr) as u32;
        let dg = g.abs_diff(tg) as u32;
        let db = b.abs_diff(tb) as u32;
        let dist = dr * dr + dg * dg + db * db;
        if dist < best_dist {
            best_dist = dist;
            best = name;
        }
    }
    best
}

/// A 256-palette index to the nearest of the 16.
pub fn indexed_to_named(n: u8) -> NamedColor {
    use NamedColor::*;
    const BASE: [NamedColor; 16] = [
        Black,
        Red,
        Green,
        Yellow,
        Blue,
        Magenta,
        Cyan,
        White,
        BrightBlack,
        BrightRed,
        BrightGreen,
        BrightYellow,
        BrightBlue,
        BrightMagenta,
        BrightCyan,
        BrightWhite,
    ];
    match n {
        0..=15 => BASE[n as usize],
        16..=231 => {
            let i = n - 16;
            let to = |v: u8| -> u8 {
                // The 6x6x6 cube's levels.
                const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
                LEVELS[v as usize]
            };
            let r = to(i / 36);
            let g = to((i % 36) / 6);
            let b = to(i % 6);
            rgb_to_named(r, g, b)
        }
        _ => {
            let level = 8 + (n - 232) as u16 * 247 / 24;
            let v = level.min(255) as u8;
            rgb_to_named(v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_color_nothing_is_emitted() {
        assert_eq!(fg_code(Color::Rgb(1, 2, 3), ColorDepth::None), None);
        assert_eq!(
            fg_code(Color::Named(NamedColor::Red), ColorDepth::None),
            None
        );
    }

    #[test]
    fn truecolor_emits_literal_rgb() {
        assert_eq!(
            fg_code(Color::Rgb(10, 20, 30), ColorDepth::TrueColor).unwrap(),
            "38;2;10;20;30"
        );
        assert_eq!(
            bg_code(Color::Rgb(10, 20, 30), ColorDepth::TrueColor).unwrap(),
            "48;2;10;20;30"
        );
    }

    #[test]
    fn rgb_degrades_to_256_and_to_16() {
        assert!(
            fg_code(Color::Rgb(255, 0, 0), ColorDepth::Ansi256)
                .unwrap()
                .starts_with("38;5;")
        );
        // Pure red must land on bright red among the 16.
        assert_eq!(
            fg_code(Color::Rgb(255, 0, 0), ColorDepth::Ansi16).unwrap(),
            "91"
        );
    }

    #[test]
    fn named_colors_translate_the_same_at_every_rung() {
        // A named color never needs quantizing: the terminal decides the shade.
        for depth in [
            ColorDepth::Ansi16,
            ColorDepth::Ansi256,
            ColorDepth::TrueColor,
        ] {
            assert_eq!(
                fg_code(Color::Named(NamedColor::Red), depth).unwrap(),
                "31",
                "rung {depth:?}"
            );
            assert_eq!(
                fg_code(Color::Named(NamedColor::BrightBlue), depth).unwrap(),
                "94"
            );
        }
    }

    #[test]
    fn background_shifts_the_code_by_ten() {
        assert_eq!(
            bg_code(Color::Named(NamedColor::Red), ColorDepth::Ansi16).unwrap(),
            "41"
        );
    }

    #[test]
    fn rgb_to_256_uses_the_grayscale_ramp() {
        let idx = rgb_to_256(128, 128, 128);
        assert!(
            (232..=255).contains(&idx),
            "a gray should land on the ramp, not the cube: {idx}"
        );
        assert_eq!(rgb_to_256(0, 0, 0), 16);
        assert_eq!(rgb_to_256(255, 255, 255), 231);
    }

    #[test]
    fn rgb_to_256_never_leaves_its_range() {
        for r in [0u8, 1, 127, 128, 254, 255] {
            for g in [0u8, 95, 175, 255] {
                for b in [0u8, 135, 215, 255] {
                    let idx = rgb_to_256(r, g, b);
                    assert!(
                        idx >= 16,
                        "({r},{g},{b}) gave {idx}, which invades the basic 16"
                    );
                }
            }
        }
    }

    #[test]
    fn indexed_to_named_covers_the_whole_range() {
        // No panics and no out-of-range indices across all 256.
        for n in 0u8..=255 {
            let _ = indexed_to_named(n);
        }
        assert_eq!(indexed_to_named(1), NamedColor::Red);
        assert_eq!(indexed_to_named(9), NamedColor::BrightRed);
    }
}
