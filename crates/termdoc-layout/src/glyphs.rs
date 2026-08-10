//! Glyph sets per Unicode level.
//!
//! These are the TABLE and BULLET rungs of the degradation ladder (docs/DESIGN.md §5):
//! `UnicodeLevel::Full` uses box-drawing, `Ascii` uses `+-|`. Keeping them in a data table
//! rather than spread across `if`s makes swapping rungs in a test trivial.

use termdoc_term::UnicodeLevel;

#[derive(Clone, Copy, Debug)]
pub struct Glyphs {
    /// Bullets by nesting depth; rotated with a modulo.
    pub bullets: [&'static str; 3],
    pub rule: &'static str,
    pub quote_bar: &'static str,
    pub task_done: &'static str,
    pub task_todo: &'static str,
    pub ellipsis: &'static str,
    pub diagnostic: &'static str,

    // Table borders.
    pub h: &'static str,
    pub v: &'static str,
    pub top_left: &'static str,
    pub top_right: &'static str,
    pub bottom_left: &'static str,
    pub bottom_right: &'static str,
    pub cross: &'static str,
    pub tee_down: &'static str,
    pub tee_up: &'static str,
    pub tee_right: &'static str,
    pub tee_left: &'static str,
}

pub const UNICODE: Glyphs = Glyphs {
    bullets: ["•", "◦", "▪"],
    rule: "─",
    quote_bar: "│",
    task_done: "☑",
    task_todo: "☐",
    ellipsis: "…",
    diagnostic: "⚠",

    h: "─",
    v: "│",
    top_left: "┌",
    top_right: "┐",
    bottom_left: "└",
    bottom_right: "┘",
    cross: "┼",
    tee_down: "┬",
    tee_up: "┴",
    tee_right: "├",
    tee_left: "┤",
};

pub const ASCII: Glyphs = Glyphs {
    bullets: ["*", "-", "+"],
    rule: "-",
    quote_bar: "|",
    task_done: "[x]",
    task_todo: "[ ]",
    ellipsis: "...",
    diagnostic: "!",

    h: "-",
    v: "|",
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    cross: "+",
    tee_down: "+",
    tee_up: "+",
    tee_right: "+",
    tee_left: "+",
};

impl Glyphs {
    pub fn for_level(level: UnicodeLevel) -> &'static Glyphs {
        match level {
            UnicodeLevel::Full => &UNICODE,
            UnicodeLevel::Ascii => &ASCII,
        }
    }

    pub fn bullet(&self, depth: u8) -> &'static str {
        self.bullets[(depth as usize) % self.bullets.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn the_ascii_glyphs_are_ascii() {
        // If a non-ASCII one slips in, the degradation rung stops degrading.
        let g = ASCII;
        for s in [
            g.rule,
            g.quote_bar,
            g.h,
            g.v,
            g.top_left,
            g.cross,
            g.ellipsis,
            g.diagnostic,
            g.task_done,
            g.task_todo,
        ] {
            assert!(s.is_ascii(), "'{s}' is not ASCII");
        }
        for b in g.bullets {
            assert!(b.is_ascii(), "bullet '{b}' is not ASCII");
        }
    }

    #[test]
    fn the_unicode_borders_take_one_cell() {
        // A two-cell border would throw the whole table out of alignment.
        let g = UNICODE;
        for s in [
            g.h,
            g.v,
            g.top_left,
            g.top_right,
            g.bottom_left,
            g.bottom_right,
            g.cross,
            g.tee_down,
            g.tee_up,
            g.tee_right,
            g.tee_left,
        ] {
            assert_eq!(UnicodeWidthStr::width(s), 1, "'{s}' is not one cell wide");
        }
    }

    #[test]
    fn bullet_rotates_without_overflowing() {
        let g = UNICODE;
        assert_eq!(g.bullet(0), g.bullet(3));
        let _ = g.bullet(250);
    }
}
