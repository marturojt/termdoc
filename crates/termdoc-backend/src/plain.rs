//! The plain-text backend.
//!
//! It exists for two distinct reasons and both matter: it is what a pipe receives
//! (`termdoc doc.md | grep`), and it is the reference against which the snapshot tests
//! check that degrading changes the appearance and not the content.

use std::io::Write;

use termdoc_core::{Backend, BackendCaps, Line, Result};

#[derive(Debug, Default)]
pub struct PlainBackend;

impl PlainBackend {
    pub fn new() -> Self {
        PlainBackend
    }
}

impl Backend for PlainBackend {
    fn caps(&self) -> BackendCaps {
        BackendCaps {
            styled: false,
            hyperlinks: false,
            graphics: false,
        }
    }

    fn write_line(&mut self, line: &Line<'_>, out: &mut dyn Write) -> Result<()> {
        for segment in &line.segments {
            out.write_all(segment.text.as_bytes())?;
        }
        out.write_all(b"\n")?;
        Ok(())
    }

    fn finish(&mut self, out: &mut dyn Write) -> Result<()> {
        out.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termdoc_core::{Color, NamedColor, Segment, Style};

    #[test]
    fn styles_are_ignored_entirely() {
        let line = Line::from_segments(
            vec![
                Segment::new("a", Style::bold().with_fg(Color::Named(NamedColor::Red))),
                Segment::plain("b"),
            ],
            2,
        );
        let mut out: Vec<u8> = Vec::new();
        let mut b = PlainBackend::new();
        b.write_line(&line, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "ab\n");
    }

    #[test]
    fn links_are_ignored() {
        let line = Line::from_segments(
            vec![Segment::plain("text").with_link("https://example.com")],
            4,
        );
        let mut out: Vec<u8> = Vec::new();
        PlainBackend::new().write_line(&line, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "text\n");
    }
}
