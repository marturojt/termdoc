//! The plain-text and log reader.
//!
//! This is the reference streaming reader: it walks the source's bytes line by line without
//! validating the whole file up front. That is why `termdoc huge.log | head -5` reads a few
//! pages rather than two gigabytes, which is the yardstick for whether this behaves like a
//! Unix utility.

use termdoc_core::{
    Diagnostic, DocumentReader, Event, Events, FormatId, Metadata, ReadContext, ReaderCaps, Result,
    Source, Span, Spanned, Tag, TagKind,
};

#[derive(Debug, Clone, Copy)]
pub struct TextReader {
    format: FormatId,
}

impl TextReader {
    pub fn plain() -> Self {
        TextReader {
            format: FormatId::PlainText,
        }
    }

    /// Logs share this reader: their lines must not be reflowed either. Highlighting by
    /// severity level arrives in M1, when the dedicated log reader exists.
    pub fn log() -> Self {
        TextReader {
            format: FormatId::Log,
        }
    }
}

impl DocumentReader for TextReader {
    fn id(&self) -> FormatId {
        self.format
    }

    fn capabilities(&self) -> ReaderCaps {
        ReaderCaps {
            streaming: true,
            paginated: false,
            metadata: false,
        }
    }

    fn read<'a>(&self, src: &'a Source, _ctx: &ReadContext) -> Result<Events<'a>> {
        Ok(Box::new(TextEvents {
            src,
            bytes: src.bytes(),
            pos: 0,
            line: 0,
            phase: Phase::Document,
            format: self.format,
            warned_encoding: false,
            pending: None,
        }))
    }
}

#[derive(Debug, PartialEq)]
enum Phase {
    Document,
    Body,
    Lines,
    EndBody,
    EndDocument,
    Done,
}

struct TextEvents<'a> {
    /// The source is kept so decoding goes through it.
    ///
    /// The reader must not decide how bytes become text: the encoding was resolved by the
    /// detection layer and applied to the source, and duplicating that judgement here is how a
    /// latin-1 file ends up rendered with replacement characters even though the right encoding
    /// was already known.
    src: &'a Source,
    bytes: &'a [u8],
    pos: usize,
    line: u32,
    phase: Phase,
    format: FormatId,
    warned_encoding: bool,
    /// An event held back for the next `next()` call.
    ///
    /// This is needed because a badly encoded line produces *two* events: the warning and
    /// the text. Without it, the warning replaced the line and the content was lost, which
    /// is the exact opposite of "partial rendering beats total failure".
    pending: Option<Spanned<Event<'a>>>,
}

impl<'a> Iterator for TextEvents<'a> {
    type Item = Result<Spanned<Event<'a>>>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(ev) = self.pending.take() {
                return Some(Ok(ev));
            }
            match self.phase {
                Phase::Document => {
                    self.phase = Phase::Body;
                    let meta = Metadata {
                        source_format: Some(self.format),
                        ..Metadata::default()
                    };
                    return Some(Ok(Spanned::bare(Event::Start(Tag::Document(Box::new(
                        meta,
                    ))))));
                }
                Phase::Body => {
                    self.phase = Phase::Lines;
                    return Some(Ok(Spanned::bare(Event::Start(Tag::Preformatted))));
                }
                Phase::Lines => {
                    if self.pos >= self.bytes.len() {
                        self.phase = Phase::EndBody;
                        continue;
                    }

                    let start = self.pos;
                    let rest = &self.bytes[start..];
                    let (line_bytes, consumed) = match rest.iter().position(|b| *b == b'\n') {
                        Some(nl) => (&rest[..nl], nl + 1),
                        None => (rest, rest.len()),
                    };
                    self.pos += consumed;
                    self.line += 1;

                    // Strip the CR from Windows line endings.
                    let line_bytes = line_bytes.strip_suffix(b"\r").unwrap_or(line_bytes);

                    // Decoding per line, through the source: valid UTF-8 is borrowed, and only
                    // a line that needs transcoding is copied. That keeps the reader lazy while
                    // still honoring whatever encoding was detected.
                    let span = Span::at_line(start as u64, self.pos as u64, self.line);
                    let (decoded, had_errors) = self.src.decode_line(line_bytes);

                    if !had_errors {
                        return Some(Ok(Spanned::new(Event::Text(decoded), span)));
                    }

                    let text = Spanned::new(Event::Text(decoded), span);
                    if self.warned_encoding {
                        return Some(Ok(text));
                    }
                    // Warn exactly once —one warning per line in a million-line log is worse
                    // than the problem— and still emit the line on the next `next()`.
                    self.warned_encoding = true;
                    self.pending = Some(text);
                    return Some(Ok(Spanned::new(
                        Event::Diagnostic(Diagnostic::warning(format!(
                            "line {} is not valid {}; shown with replacements",
                            self.line,
                            self.src.encoding_name()
                        ))),
                        span,
                    )));
                }
                Phase::EndBody => {
                    self.phase = Phase::EndDocument;
                    return Some(Ok(Spanned::bare(Event::End(TagKind::Preformatted))));
                }
                Phase::EndDocument => {
                    self.phase = Phase::Done;
                    return Some(Ok(Spanned::bare(Event::End(TagKind::Document))));
                }
                Phase::Done => return None,
            }
        }
    }
}

impl std::fmt::Debug for TextEvents<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextEvents")
            .field("pos", &self.pos)
            .field("line", &self.line)
            .field("phase", &self.phase)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    fn collect_events(src: &Source) -> Vec<Event<'_>> {
        TextReader::plain()
            .read(src, &ReadContext::default())
            .unwrap()
            .map(|e| e.unwrap().node)
            .collect()
    }

    fn texts(src: &Source) -> Vec<String> {
        collect_events(src)
            .into_iter()
            .filter_map(|e| match e {
                Event::Text(t) => Some(t.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn wraps_the_content_in_document_and_preformatted() {
        let src = Source::from_bytes("t", "hello");
        let ev = collect_events(&src);
        assert!(matches!(ev[0], Event::Start(Tag::Document(_))));
        assert!(matches!(ev[1], Event::Start(Tag::Preformatted)));
        assert!(matches!(
            ev[ev.len() - 2],
            Event::End(TagKind::Preformatted)
        ));
        assert!(matches!(ev[ev.len() - 1], Event::End(TagKind::Document)));
    }

    #[test]
    fn one_line_per_text_event() {
        let src = Source::from_bytes("t", "one\ntwo\nthree");
        assert_eq!(texts(&src), vec!["one", "two", "three"]);
    }

    #[test]
    fn the_trailing_newline_does_not_create_an_extra_line() {
        // `printf 'a\nb\n'` is two lines, not three.
        let src = Source::from_bytes("t", "a\nb\n");
        assert_eq!(texts(&src), vec!["a", "b"]);
    }

    #[test]
    fn strips_windows_carriage_returns() {
        let src = Source::from_bytes("t", "one\r\ntwo\r\n");
        assert_eq!(texts(&src), vec!["one", "two"]);
    }

    #[test]
    fn valid_text_is_never_copied() {
        // The streaming reader's memory invariant.
        let src = Source::from_bytes("t", "one\ntwo");
        for e in collect_events(&src) {
            if let Event::Text(t) = e {
                assert!(
                    matches!(t, Cow::Borrowed(_)),
                    "'{t}' was copied to the heap"
                );
            }
        }
    }

    #[test]
    fn warns_about_encoding_exactly_once() {
        let mut bytes = Vec::new();
        for _ in 0..5 {
            bytes.extend_from_slice(&[0xff, 0xfe, b'\n']);
        }
        let src = Source::from_bytes("t", bytes);
        let diags = collect_events(&src)
            .into_iter()
            .filter(|e| matches!(e, Event::Diagnostic(_)))
            .count();
        assert_eq!(diags, 1, "one warning per line would flood a large log");
    }

    #[test]
    fn broken_lines_are_still_shown() {
        let src = Source::from_bytes("t", vec![0xff, b'a', b'\n', b'b']);
        let t = texts(&src);
        assert!(t.iter().any(|l| l.contains('a')), "{t:?}");
        assert!(t.iter().any(|l| l == "b"), "{t:?}");
    }

    #[test]
    fn the_configured_encoding_is_honored() {
        // Regression: the reader validated UTF-8 itself and ignored the encoding the detection
        // layer had already resolved, so a latin-1 file rendered as replacement characters even
        // though the right answer was known.
        let mut src = Source::from_bytes("t", b"Comit\xE9\nse\xF1or\n".to_vec());
        src.set_encoding("windows-1252").unwrap();
        assert_eq!(texts(&src), vec!["Comité", "señor"]);
        assert!(
            !collect_events(&src)
                .iter()
                .any(|e| matches!(e, Event::Diagnostic(_))),
            "a correctly decoded file must produce no warning"
        );
    }

    #[test]
    fn spans_carry_the_line_number() {
        let src = Source::from_bytes("t", "one\ntwo");
        let spanned: Vec<_> = TextReader::plain()
            .read(&src, &ReadContext::default())
            .unwrap()
            .map(|e| e.unwrap())
            .filter(|s| matches!(s.node, Event::Text(_)))
            .collect();
        assert_eq!(spanned[0].span.line, Some(1));
        assert_eq!(spanned[1].span.line, Some(2));
        assert_eq!(spanned[0].span.start, 0);
    }

    #[test]
    fn an_empty_source_produces_an_empty_document() {
        let src = Source::from_bytes("t", "");
        assert!(texts(&src).is_empty());
        // But the structure is still well formed.
        assert_eq!(collect_events(&src).len(), 4);
    }

    #[test]
    fn it_is_lazy() {
        // Consuming three events must not have walked the whole file. This is the property
        // that keeps `| head -5` from reading gigabytes.
        let big = "x".repeat(100_000) + "\n" + &"y\n".repeat(1000);
        let src = Source::from_bytes("t", big);
        let mut it = TextReader::plain()
            .read(&src, &ReadContext::default())
            .unwrap();
        it.next();
        it.next();
        let third = it.next().unwrap().unwrap();
        assert_eq!(
            third.span.line,
            Some(1),
            "should still be on the first line"
        );
    }
}
