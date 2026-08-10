//! The layout's output vocabulary: laid-out lines, ready for a backend.
//!
//! This lives in `core` rather than in `layout` for the same reason `Event` does: it is
//! the interface *between* two stages. `Event` is the reader→layout contract; `Line` is
//! the layout→backend contract. Both belong to the shared vocabulary, and putting them
//! here is what lets `termdoc-backend` avoid depending on `termdoc-layout`
//! (docs/DESIGN.md §3).

use std::borrow::Cow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    /// A literal color. The backend quantizes it down to 256/16 when the terminal
    /// cannot represent it.
    Rgb(u8, u8, u8),
    /// An index into the 256-color palette.
    Indexed(u8),
    /// One of the 16 ANSI names. The terminal picks the actual shade, so this respects
    /// the user's own theme — which is why it is the preferred default.
    Named(NamedColor),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamedColor {
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
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub reverse: bool,
}

impl Style {
    pub const PLAIN: Style = Style {
        fg: None,
        bg: None,
        bold: false,
        dim: false,
        italic: false,
        underline: false,
        strike: false,
        reverse: false,
    };

    pub const fn fg(color: Color) -> Self {
        Style {
            fg: Some(color),
            ..Style::PLAIN
        }
    }

    pub const fn bold() -> Self {
        Style {
            bold: true,
            ..Style::PLAIN
        }
    }

    pub const fn with_bold(mut self) -> Self {
        self.bold = true;
        self
    }

    pub const fn with_dim(mut self) -> Self {
        self.dim = true;
        self
    }

    pub const fn with_italic(mut self) -> Self {
        self.italic = true;
        self
    }

    pub const fn with_underline(mut self) -> Self {
        self.underline = true;
        self
    }

    pub const fn with_strike(mut self) -> Self {
        self.strike = true;
        self
    }

    pub const fn with_fg(mut self, color: Color) -> Self {
        self.fg = Some(color);
        self
    }

    /// Layers `other` on top of `self`: boolean attributes accumulate and `other`'s
    /// colors win when it defines them. This is what makes `**_bold italic_**` work by
    /// stacking styles instead of replacing them.
    pub fn over(self, other: Style) -> Style {
        Style {
            fg: other.fg.or(self.fg),
            bg: other.bg.or(self.bg),
            bold: self.bold || other.bold,
            dim: self.dim || other.dim,
            italic: self.italic || other.italic,
            underline: self.underline || other.underline,
            strike: self.strike || other.strike,
            reverse: self.reverse || other.reverse,
        }
    }

    pub fn is_plain(&self) -> bool {
        *self == Style::PLAIN
    }
}

/// A contiguous run of text sharing a single style.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment<'a> {
    pub text: Cow<'a, str>,
    pub style: Style,
    /// Hyperlink target, when this run is part of one. The backend emits it as OSC 8 or
    /// degrades it to a numbered reference.
    pub link: Option<Cow<'a, str>>,
}

impl<'a> Segment<'a> {
    pub fn new(text: impl Into<Cow<'a, str>>, style: Style) -> Self {
        Segment {
            text: text.into(),
            style,
            link: None,
        }
    }

    pub fn plain(text: impl Into<Cow<'a, str>>) -> Self {
        Segment::new(text, Style::PLAIN)
    }

    pub fn with_link(mut self, href: impl Into<Cow<'a, str>>) -> Self {
        self.link = Some(href.into());
        self
    }
}

/// One line of output. `width` is the display-cell width already computed by the layout;
/// the backend does not measure again.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Line<'a> {
    pub segments: Vec<Segment<'a>>,
    pub width: usize,
}

impl<'a> Line<'a> {
    pub fn empty() -> Self {
        Line::default()
    }

    pub fn from_segments(segments: Vec<Segment<'a>>, width: usize) -> Self {
        Line { segments, width }
    }

    /// The text with styles dropped. The basis of the plain backend and of test asserts.
    pub fn to_plain_string(&self) -> String {
        self.segments.iter().map(|s| s.text.as_ref()).collect()
    }

    pub fn is_blank(&self) -> bool {
        self.segments
            .iter()
            .all(|s| s.text.chars().all(char::is_whitespace))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn over_accumulates_attributes_and_prefers_the_new_color() {
        let base = Style::bold().with_fg(Color::Named(NamedColor::Red));
        let top = Style::PLAIN
            .with_italic()
            .with_fg(Color::Named(NamedColor::Blue));
        let merged = base.over(top);
        assert!(merged.bold, "the base's bold must survive");
        assert!(merged.italic);
        assert_eq!(merged.fg, Some(Color::Named(NamedColor::Blue)));
    }

    #[test]
    fn over_keeps_the_base_color_when_the_new_one_is_unset() {
        let base = Style::fg(Color::Named(NamedColor::Red));
        let merged = base.over(Style::bold());
        assert_eq!(merged.fg, Some(Color::Named(NamedColor::Red)));
        assert!(merged.bold);
    }

    #[test]
    fn to_plain_string_concatenates() {
        let line = Line::from_segments(
            vec![Segment::plain("ab"), Segment::new("cd", Style::bold())],
            4,
        );
        assert_eq!(line.to_plain_string(), "abcd");
    }
}
