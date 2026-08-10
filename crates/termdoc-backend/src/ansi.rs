//! The ANSI backend.
//!
//! Invariants the tests pin down, because these are what make the output safe to
//! redirect, to page and to copy:
//!
//! 1. **Every line ends with no style active.** No attribute leaks into the next line, or
//!    into the user's prompt if the output is interrupted.
//! 2. **No redundant sequences.** The current style is backend state, so a whole
//!    paragraph in one color emits a single SGR.
//! 3. **With `ColorDepth::None`, not one byte of color escape is emitted**, even when the
//!    layout asked for colors.

use std::io::Write;

use termdoc_core::{Backend, BackendCaps, Line, Result, Style};
use termdoc_term::{ColorDepth, Fidelity};

use crate::color::{bg_code, fg_code};

const RESET: &str = "\x1b[0m";

#[derive(Debug)]
pub struct AnsiBackend {
    fidelity: Fidelity,
    /// The style currently active in the terminal. This is what lets us skip repeating
    /// sequences.
    current: Style,
    link_open: bool,
}

impl AnsiBackend {
    pub fn new(fidelity: Fidelity) -> Self {
        AnsiBackend {
            fidelity,
            current: Style::PLAIN,
            link_open: false,
        }
    }

    /// The full SGR sequence for a style, always starting from a reset.
    ///
    /// Resetting instead of computing the minimal transition is deliberate: turning
    /// individual attributes off (22 for bold/dim, 23 for italic…) is poorly implemented
    /// in a fair number of terminals, whereas a reset followed by the desired attributes
    /// behaves identically everywhere.
    fn sgr(&self, style: Style) -> String {
        if style.is_plain() {
            return RESET.to_string();
        }

        let mut codes: Vec<String> = Vec::new();
        if style.bold {
            codes.push("1".into());
        }
        if style.dim {
            codes.push("2".into());
        }
        if style.italic {
            codes.push("3".into());
        }
        if style.underline {
            codes.push("4".into());
        }
        if style.reverse {
            codes.push("7".into());
        }
        if style.strike {
            codes.push("9".into());
        }
        if let Some(fg) = style.fg
            && let Some(code) = fg_code(fg, self.fidelity.color)
        {
            codes.push(code);
        }
        if let Some(bg) = style.bg
            && let Some(code) = bg_code(bg, self.fidelity.color)
        {
            codes.push(code);
        }

        if codes.is_empty() {
            // Can happen when the style only asked for color and we have none.
            RESET.to_string()
        } else {
            format!("\x1b[0;{}m", codes.join(";"))
        }
    }

    /// Strips from the style whatever this rung cannot express.
    ///
    /// Without this, a style that only asked for color would emit a pointless reset at
    /// `ColorDepth::None` and leak escapes into output that must stay clean. Bold and
    /// underline survive: they are the lowest color rung that still distinguishes
    /// emphasis.
    fn effective(&self, mut style: Style) -> Style {
        if self.fidelity.color == ColorDepth::None {
            style.fg = None;
            style.bg = None;
        }
        style
    }

    fn apply(&mut self, style: Style, out: &mut dyn Write) -> Result<()> {
        let style = self.effective(style);
        if style == self.current {
            return Ok(());
        }
        out.write_all(self.sgr(style).as_bytes())?;
        self.current = style;
        Ok(())
    }

    fn reset(&mut self, out: &mut dyn Write) -> Result<()> {
        if !self.current.is_plain() {
            out.write_all(RESET.as_bytes())?;
            self.current = Style::PLAIN;
        }
        Ok(())
    }

    fn open_link(&mut self, href: &str, out: &mut dyn Write) -> Result<()> {
        write!(out, "\x1b]8;;{href}\x1b\\")?;
        self.link_open = true;
        Ok(())
    }

    fn close_link(&mut self, out: &mut dyn Write) -> Result<()> {
        if self.link_open {
            out.write_all(b"\x1b]8;;\x1b\\")?;
            self.link_open = false;
        }
        Ok(())
    }
}

impl Backend for AnsiBackend {
    fn caps(&self) -> BackendCaps {
        BackendCaps {
            styled: self.fidelity.color != ColorDepth::None,
            hyperlinks: self.fidelity.hyperlinks,
            graphics: false,
        }
    }

    fn write_line(&mut self, line: &Line<'_>, out: &mut dyn Write) -> Result<()> {
        let mut pending_link: Option<&str> = None;

        for segment in &line.segments {
            let want_link = if self.fidelity.hyperlinks {
                segment.link.as_deref()
            } else {
                None
            };

            // The link is only reopened when it changes: a multi-word link must be a
            // single clickable target, not one per word.
            if want_link != pending_link {
                self.close_link(out)?;
                if let Some(href) = want_link {
                    self.open_link(href, out)?;
                }
                pending_link = want_link;
            }

            self.apply(segment.style, out)?;
            out.write_all(segment.text.as_bytes())?;
        }

        self.close_link(out)?;
        // Reset before the newline: this is what guarantees invariant 1.
        self.reset(out)?;
        out.write_all(b"\n")?;
        Ok(())
    }

    fn finish(&mut self, out: &mut dyn Write) -> Result<()> {
        self.close_link(out)?;
        self.reset(out)?;
        out.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termdoc_core::{Color, NamedColor, Segment};

    fn render(lines: &[Line<'_>], fidelity: Fidelity) -> String {
        let mut b = AnsiBackend::new(fidelity);
        let mut out: Vec<u8> = Vec::new();
        b.begin(&mut out).unwrap();
        for l in lines {
            b.write_line(l, &mut out).unwrap();
        }
        b.finish(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn styled_line() -> Line<'static> {
        Line::from_segments(
            vec![
                Segment::plain("normal "),
                Segment::new("red", Style::fg(Color::Named(NamedColor::Red))),
            ],
            10,
        )
    }

    #[test]
    fn no_style_survives_a_newline() {
        let out = render(&[styled_line(), styled_line()], Fidelity::FULL);

        // We do not assert that the line *ends* with a reset — it may legitimately end
        // with unstyled text — but that the last SGR on the line is a reset. Every SGR
        // the backend emits starts from zero, so that final sequence determines the state
        // carried into the next line.
        for line in out.lines().filter(|l| l.contains('\x1b')) {
            let last = line
                .rmatch_indices("\x1b[")
                .next()
                .map(|(i, _)| &line[i..])
                .expect("there is at least one sequence");
            assert!(
                last.starts_with(RESET),
                "the last SGR of '{}' is not a reset: '{}'",
                line.escape_debug(),
                last.escape_debug()
            );
        }
    }

    #[test]
    fn without_color_there_are_no_escapes() {
        // The invariant that makes `termdoc x.md > f` produce a clean file.
        let out = render(&[styled_line()], Fidelity::PLAIN);
        assert!(
            !out.contains('\x1b'),
            "an escape leaked through: {}",
            out.escape_debug()
        );
        assert_eq!(out, "normal red\n");
    }

    #[test]
    fn no_repeated_sequences_for_the_same_style() {
        let style = Style::fg(Color::Named(NamedColor::Red));
        let line = Line::from_segments(
            vec![
                Segment::new("a", style),
                Segment::new("b", style),
                Segment::new("c", style),
            ],
            3,
        );
        let out = render(&[line], Fidelity::FULL);
        let sgr_count = out.matches("\x1b[0;").count();
        assert_eq!(
            sgr_count,
            1,
            "expected a single SGR, got {sgr_count}: {}",
            out.escape_debug()
        );
    }

    #[test]
    fn emits_osc8_when_supported() {
        let line = Line::from_segments(
            vec![Segment::plain("click").with_link("https://example.com")],
            5,
        );
        let out = render(&[line], Fidelity::FULL);
        assert!(out.contains("\x1b]8;;https://example.com\x1b\\"));
        assert!(out.contains("\x1b]8;;\x1b\\"), "the link must be closed");
    }

    #[test]
    fn without_osc8_support_no_links_are_emitted() {
        let line = Line::from_segments(
            vec![Segment::plain("click").with_link("https://example.com")],
            5,
        );
        let mut f = Fidelity::FULL;
        f.hyperlinks = false;
        let out = render(&[line], f);
        assert!(!out.contains("\x1b]8"), "{}", out.escape_debug());
        assert!(out.contains("click"));
    }

    #[test]
    fn a_multi_word_link_is_a_single_target() {
        let href = "https://example.com";
        let line = Line::from_segments(
            vec![
                Segment::plain("two").with_link(href),
                Segment::plain(" ").with_link(href),
                Segment::plain("words").with_link(href),
            ],
            9,
        );
        let out = render(&[line], Fidelity::FULL);
        assert_eq!(
            out.matches("\x1b]8;;https://example.com").count(),
            1,
            "the link was reopened per segment: {}",
            out.escape_debug()
        );
    }

    #[test]
    fn the_text_survives_every_rung() {
        // Degrading changes how it looks, never what it says.
        for fidelity in [Fidelity::FULL, Fidelity::default(), Fidelity::PLAIN] {
            let out = render(&[styled_line()], fidelity);
            let visible: String = strip_escapes(&out);
            assert_eq!(visible, "normal red\n", "rung {fidelity:?}");
        }
    }

    fn strip_escapes(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '\x1b' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('[') => {
                    // CSI: ends with a letter.
                    while let Some(&c) = chars.peek() {
                        chars.next();
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC: ends with ST (ESC \) or BEL.
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }
}
