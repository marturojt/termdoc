//! Themes: semantic roles → styles.
//!
//! The layout never picks a color directly; it asks for the role's style. That is what will
//! let `--theme` change the appearance without touching the engine.
//!
//! The defaults use **named** colors (`NamedColor`) rather than RGB, so the terminal
//! applies its own palette and termdoc respects the user's theme instead of imposing one.

use termdoc_core::{Color, NamedColor, Style};

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub heading: [Style; 6],
    pub emphasis: Style,
    pub strong: Style,
    pub strikethrough: Style,
    pub underline: Style,
    pub highlight: Style,
    pub code_inline: Style,
    pub code_block: Style,
    pub link: Style,
    pub link_reference: Style,
    pub quote: Style,
    pub quote_bar: Style,
    pub rule: Style,
    pub list_marker: Style,
    pub table_header: Style,
    pub table_border: Style,
    pub metadata_key: Style,
    pub metadata_value: Style,
    pub diagnostic_warning: Style,
    pub diagnostic_error: Style,
    pub image_placeholder: Style,
    pub footnote_ref: Style,
}

impl Default for Theme {
    fn default() -> Self {
        use NamedColor::*;
        let c = Color::Named;
        Theme {
            heading: [
                Style::bold().with_fg(c(BrightBlue)),
                Style::bold().with_fg(c(BrightCyan)),
                Style::bold().with_fg(c(BrightGreen)),
                Style::bold().with_fg(c(BrightYellow)),
                Style::bold().with_fg(c(BrightMagenta)),
                Style::bold().with_fg(c(White)),
            ],
            emphasis: Style::PLAIN.with_italic(),
            strong: Style::bold(),
            strikethrough: Style::PLAIN.with_strike(),
            underline: Style::PLAIN.with_underline(),
            highlight: Style::PLAIN.with_fg(c(Black)),
            code_inline: Style::fg(c(BrightYellow)),
            code_block: Style::fg(c(BrightYellow)),
            link: Style::fg(c(BrightBlue)).with_underline(),
            link_reference: Style::fg(c(Blue)).with_dim(),
            quote: Style::PLAIN.with_italic().with_dim(),
            quote_bar: Style::fg(c(BrightBlack)),
            rule: Style::fg(c(BrightBlack)),
            list_marker: Style::fg(c(BrightCyan)),
            table_header: Style::bold(),
            table_border: Style::fg(c(BrightBlack)),
            metadata_key: Style::fg(c(BrightBlack)),
            metadata_value: Style::PLAIN,
            diagnostic_warning: Style::fg(c(BrightYellow)),
            diagnostic_error: Style::fg(c(BrightRed)).with_bold(),
            image_placeholder: Style::fg(c(BrightMagenta)).with_dim(),
            footnote_ref: Style::fg(c(BrightBlue)).with_dim(),
        }
    }
}

impl Theme {
    /// A theme with no color, for the plain backend and the degradation tests.
    pub fn plain() -> Self {
        Theme {
            heading: [Style::PLAIN; 6],
            emphasis: Style::PLAIN,
            strong: Style::PLAIN,
            strikethrough: Style::PLAIN,
            underline: Style::PLAIN,
            highlight: Style::PLAIN,
            code_inline: Style::PLAIN,
            code_block: Style::PLAIN,
            link: Style::PLAIN,
            link_reference: Style::PLAIN,
            quote: Style::PLAIN,
            quote_bar: Style::PLAIN,
            rule: Style::PLAIN,
            list_marker: Style::PLAIN,
            table_header: Style::PLAIN,
            table_border: Style::PLAIN,
            metadata_key: Style::PLAIN,
            metadata_value: Style::PLAIN,
            diagnostic_warning: Style::PLAIN,
            diagnostic_error: Style::PLAIN,
            image_placeholder: Style::PLAIN,
            footnote_ref: Style::PLAIN,
        }
    }

    /// A heading's style, saturating at level 6.
    pub fn heading_style(&self, level: u8) -> Style {
        let idx = (level.clamp(1, 6) - 1) as usize;
        self.heading[idx]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_style_saturates_at_both_ends() {
        let t = Theme::default();
        assert_eq!(t.heading_style(0), t.heading_style(1));
        assert_eq!(t.heading_style(9), t.heading_style(6));
    }

    #[test]
    fn the_plain_theme_has_no_styles() {
        let t = Theme::plain();
        assert!(t.strong.is_plain());
        assert!(t.link.is_plain());
        for h in t.heading {
            assert!(h.is_plain());
        }
    }
}
