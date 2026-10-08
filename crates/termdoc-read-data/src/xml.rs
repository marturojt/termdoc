//! The XML reader.
//!
//! # How it uses `quick-xml`
//!
//! Unlike YAML and TOML (see `yaml.rs`), XML has a pull parser that loses nothing: `quick-xml`
//! reports comments, CDATA, declarations and processing instructions as events, and says how
//! many bytes each one covered. This reader leans on exactly that and no more. The parser is
//! used to **delimit** — where one tag, comment or run of text ends — and the bytes of that
//! range go out untouched. The parser's own idea of the content (unescaped text, a normalised
//! tag) is never shown, so the output is the file on disk, only coloured.
//!
//! The parser is asked for one event at a time, over what is left of the input, and a new
//! `Reader` is made for each. That costs nothing (a slice reader holds no buffers) and keeps
//! this iterator free of the self-reference a long-lived `Reader` borrowing the source would
//! need.
//!
//! Inside a tag the parser has already found the closing `>` (it knows `a="x>y"` does not end
//! it), so colouring the name, the attributes and their values is a few lines over a range that
//! is known to be one tag.
//!
//! # Streaming
//!
//! Real. Events are produced on demand and borrow from the source, so `termdoc big.xml | head`
//! reads the start of the file. A long text node or comment is emitted a line at a time, so
//! its size does not become memory either. The exception is a source that is not UTF-8: it has
//! to be transcoded as a whole first (`Source::as_str`), once, and the result is cached in the
//! source.
//!
//! # What happens when it is not well-formed
//!
//! Partial rendering beats refusing: on the first error the reader says so with a warning and
//! shows everything from that point on uncoloured, so no byte is lost. Mismatched and stray
//! end tags are not errors here — a viewer has no business rejecting them.
//!
//! # What it does not do
//!
//! It does not re-indent. A one-line SOAP response stays one line. Unlike JSON, XML whitespace
//! can be content, and showing the file as it is is the safe default.

use std::borrow::Cow;
use std::collections::VecDeque;

use quick_xml::Reader;
use quick_xml::events::Event as XmlEvent;
use termdoc_core::{
    Diagnostic, DocumentReader, Event, Events, FormatId, Metadata, ReadContext, ReaderCaps, Result,
    Source, Span, Spanned, Tag, TagKind, TokenRole,
};

use termdoc_core::highlight::{Painter, Piece};

#[derive(Debug, Clone, Copy, Default)]
pub struct XmlReader;

impl XmlReader {
    pub fn new() -> Self {
        XmlReader
    }
}

impl DocumentReader for XmlReader {
    fn id(&self) -> FormatId {
        FormatId::Xml
    }

    fn capabilities(&self) -> ReaderCaps {
        ReaderCaps {
            // True: events are pulled on demand and borrowed from the source.
            streaming: true,
            paginated: false,
            metadata: false,
        }
    }

    fn read<'a>(&self, src: &'a Source, _ctx: &ReadContext) -> Result<Events<'a>> {
        Ok(Box::new(XmlEvents::new(src)))
    }
}

struct XmlEvents<'a> {
    hay: &'a [u8],
    /// Whether `hay` is the source's own bytes, which may hold invalid UTF-8, or text that was
    /// already decoded and is valid by construction.
    raw: bool,
    pos: usize,
    line: u32,
    pending: VecDeque<Spanned<Event<'a>>>,
    warned_encoding: bool,
    done: bool,
}

impl<'a> XmlEvents<'a> {
    fn new(src: &'a Source) -> Self {
        // UTF-8 is read in place, so nothing walks the file up front. Anything else has to be
        // transcoded whole, which `as_str` does once and caches.
        let utf8 = src.encoding_name().eq_ignore_ascii_case("utf-8");
        let hay: &'a [u8] = if utf8 {
            src.bytes()
        } else {
            src.as_str().0.as_bytes()
        };

        let mut pending = VecDeque::new();
        pending.push_back(Spanned::bare(Event::Start(Tag::Document(Box::new(
            Metadata {
                source_format: Some(FormatId::Xml),
                ..Metadata::default()
            },
        )))));
        pending.push_back(Spanned::bare(Event::Start(Tag::Preformatted)));
        XmlEvents {
            hay,
            raw: utf8,
            pos: 0,
            line: 1,
            pending,
            warned_encoding: false,
            done: false,
        }
    }

    /// Pulls one markup event and queues its coloured text.
    fn fill(&mut self) {
        if self.pos >= self.hay.len() {
            self.finish();
            return;
        }

        let rest = &self.hay[self.pos..];
        let mut reader = Reader::from_reader(rest);
        let cfg = reader.config_mut();
        // A viewer does not reject a file for a mismatched or stray end tag.
        cfg.check_end_names = false;
        cfg.allow_unmatched_ends = true;

        let (len, pieces) = match reader.read_event() {
            Ok(XmlEvent::Eof) => {
                self.finish();
                return;
            }
            Ok(event) => {
                let len = reader.buffer_position() as usize;
                if len == 0 || len > rest.len() {
                    // A parser that makes no progress would loop forever.
                    return self.give_up(
                        "not well-formed XML (the parser stopped making progress); \
                         the rest is shown uncoloured"
                            .into(),
                    );
                }
                (len, lex_event(&event, &rest[..len]))
            }
            // The parser refuses invalid UTF-8 outright. That is a different problem from bad
            // markup, and `text` would warn about it a second time for the same bytes.
            Err(quick_xml::Error::Encoding(_)) => {
                self.warned_encoding = true;
                return self.give_up(format!(
                    "line {} is not valid UTF-8; the rest is shown uncoloured, with replacements",
                    self.line
                ));
            }
            Err(e) => {
                return self.give_up(format!(
                    "not well-formed XML ({e}); the rest is shown uncoloured"
                ));
            }
        };

        self.emit(self.pos, pieces);
        self.pos += len;
    }

    /// Reports a problem once, then shows the rest of the input as it is.
    fn give_up(&mut self, message: String) {
        let start = self.pos;
        self.pending.push_back(Spanned::new(
            Event::Diagnostic(Diagnostic::warning(message)),
            Span::at_line(start as u64, self.hay.len() as u64, self.line),
        ));
        let rest_len = self.hay.len() - start;
        self.emit(start, vec![(0..rest_len, None)]);
        self.pos = self.hay.len();
    }

    fn finish(&mut self) {
        self.pending
            .push_back(Spanned::bare(Event::End(TagKind::Preformatted)));
        self.pending
            .push_back(Spanned::bare(Event::End(TagKind::Document)));
        self.done = true;
    }

    /// Queues `pieces` (ranges relative to `base`) as events, a line at a time: a long text
    /// node or comment never becomes one big event.
    fn emit(&mut self, base: usize, pieces: Vec<Piece>) {
        for (range, role) in pieces {
            let from = base + range.start;
            let to = base + range.end;
            if let Some(role) = role {
                self.queue(Event::Start(Tag::Token { role }), from, to);
            }
            let mut at = from;
            while at < to {
                let end = match self.hay[at..to].iter().position(|b| *b == b'\n') {
                    Some(nl) => at + nl + 1,
                    None => to,
                };
                let text = self.text(at, end);
                self.queue(Event::Text(text), at, end);
                if self.hay[end - 1] == b'\n' {
                    self.line += 1;
                }
                at = end;
            }
            if role.is_some() {
                self.queue(Event::End(TagKind::Token), to, to);
            }
        }
    }

    fn queue(&mut self, event: Event<'a>, from: usize, to: usize) {
        self.pending.push_back(Spanned::new(
            event,
            Span::at_line(from as u64, to as u64, self.line),
        ));
    }

    /// The text of a byte range, borrowed unless it holds invalid UTF-8.
    fn text(&mut self, from: usize, to: usize) -> Cow<'a, str> {
        let bytes: &'a [u8] = &self.hay[from..to];
        match std::str::from_utf8(bytes) {
            Ok(s) => Cow::Borrowed(s),
            Err(_) => {
                debug_assert!(self.raw, "decoded text is valid UTF-8 by construction");
                if !self.warned_encoding {
                    self.warned_encoding = true;
                    self.pending.push_back(Spanned::new(
                        Event::Diagnostic(Diagnostic::warning(format!(
                            "line {} is not valid UTF-8; shown with replacements",
                            self.line
                        ))),
                        Span::at_line(from as u64, to as u64, self.line),
                    ));
                }
                Cow::Owned(String::from_utf8_lossy(bytes).into_owned())
            }
        }
    }
}

impl<'a> Iterator for XmlEvents<'a> {
    type Item = Result<Spanned<Event<'a>>>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(Ok(event));
            }
            if self.done {
                return None;
            }
            self.fill();
        }
    }
}

// ---------------------------------------------------------------------------- colouring

/// Colours the bytes of one markup event. The result covers `0..raw.len()` exactly.
fn lex_event(event: &XmlEvent<'_>, raw: &[u8]) -> Vec<Piece> {
    let whole = |role| vec![(0..raw.len(), role)];
    match event {
        XmlEvent::Start(_) | XmlEvent::Empty(_) | XmlEvent::End(_) | XmlEvent::Decl(_) => {
            lex_markup(raw)
        }
        XmlEvent::PI(_) => lex_markup(raw),
        XmlEvent::Comment(_) => whole(Some(TokenRole::Comment)),
        XmlEvent::DocType(_) => whole(Some(TokenRole::Attribute)),
        XmlEvent::CData(_) => {
            const OPEN: &[u8] = b"<![CDATA[";
            const CLOSE: &[u8] = b"]]>";
            if raw.starts_with(OPEN)
                && raw.ends_with(CLOSE)
                && raw.len() >= OPEN.len() + CLOSE.len()
            {
                let end = raw.len() - CLOSE.len();
                vec![
                    (0..OPEN.len(), Some(TokenRole::Punctuation)),
                    (OPEN.len()..end, Some(TokenRole::String)),
                    (end..raw.len(), Some(TokenRole::Punctuation)),
                ]
            } else {
                whole(Some(TokenRole::String))
            }
        }
        // Text, entity references and anything else are the document's own words.
        _ => whole(None),
    }
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | b'\n')
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && is_space(b[i]) {
        i += 1;
    }
    i
}

/// One tag, declaration or processing instruction: `<name a="v" b='w'>`, `</name>`,
/// `<name/>`, `<?target a="v"?>`. The range is already known to be a single piece of markup.
fn lex_markup(b: &[u8]) -> Vec<Piece> {
    let len = b.len();
    let mut p = Painter::new();

    let mut i = if b.starts_with(b"<?") || b.starts_with(b"</") {
        2
    } else {
        usize::from(b.starts_with(b"<"))
    };
    p.tok(0, i, TokenRole::Punctuation);

    let name = i;
    while i < len && !is_space(b[i]) && !matches!(b[i], b'/' | b'>' | b'?') {
        i += 1;
    }
    p.tok(name, i, TokenRole::Name);

    while i < len {
        i = skip_ws(b, i);
        if i >= len {
            break;
        }
        if b[i..].starts_with(b"?>") || b[i..].starts_with(b"/>") {
            p.tok(i, i + 2, TokenRole::Punctuation);
            break;
        }
        if b[i] == b'>' {
            p.tok(i, i + 1, TokenRole::Punctuation);
            break;
        }

        let start = i;
        while i < len && !is_space(b[i]) && !matches!(b[i], b'=' | b'>' | b'/' | b'?') {
            i += 1;
        }
        if i == start {
            // A stray byte that is none of the above: left plain, but always stepped over.
            i += 1;
            continue;
        }
        p.tok(start, i, TokenRole::Attribute);

        i = skip_ws(b, i);
        if i < len && b[i] == b'=' {
            p.tok(i, i + 1, TokenRole::Punctuation);
            i = skip_ws(b, i + 1);
            if i < len && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                let end = b[i + 1..]
                    .iter()
                    .position(|c| *c == q)
                    .map_or(len, |x| i + 1 + x + 1);
                p.tok(i, end, TokenRole::String);
                i = end;
            } else {
                let vstart = i;
                while i < len && !is_space(b[i]) && b[i] != b'>' {
                    i += 1;
                }
                p.tok(vstart, i, TokenRole::String);
            }
        }
    }
    p.finish(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(markup: &str) -> String {
        lex_markup(markup.as_bytes())
            .into_iter()
            .map(|(r, role)| {
                let t = &markup[r];
                match role {
                    Some(role) => format!("{{{role:?}|{t}}}"),
                    None => t.to_string(),
                }
            })
            .collect()
    }

    #[test]
    fn a_tag_has_a_name_attributes_and_values() {
        assert_eq!(
            show(r#"<a href="x" id='y'>"#),
            "{Punctuation|<}{Name|a} {Attribute|href}{Punctuation|=}{String|\"x\"} \
             {Attribute|id}{Punctuation|=}{String|'y'}{Punctuation|>}"
        );
    }

    #[test]
    fn end_and_empty_tags() {
        assert_eq!(show("</a>"), "{Punctuation|</}{Name|a}{Punctuation|>}");
        assert_eq!(show("<br/>"), "{Punctuation|<}{Name|br}{Punctuation|/>}");
        assert_eq!(show("<br />"), "{Punctuation|<}{Name|br} {Punctuation|/>}");
    }

    #[test]
    fn a_closing_angle_bracket_inside_a_value_does_not_end_the_tag() {
        assert_eq!(
            show(r#"<a b="x>y">"#),
            "{Punctuation|<}{Name|a} {Attribute|b}{Punctuation|=}{String|\"x>y\"}{Punctuation|>}"
        );
    }

    #[test]
    fn a_declaration_and_a_processing_instruction() {
        assert_eq!(
            show(r#"<?xml version="1.0"?>"#),
            "{Punctuation|<?}{Name|xml} {Attribute|version}{Punctuation|=}{String|\"1.0\"}\
             {Punctuation|?>}"
        );
        assert_eq!(
            show("<?php echo 1 ?>"),
            "{Punctuation|<?}{Name|php} {Attribute|echo} {Attribute|1} {Punctuation|?>}"
        );
    }

    #[test]
    fn a_tag_can_span_lines_and_have_a_prefixed_name() {
        assert_eq!(
            show("<ns:a\n  x:y=\"1\"\n>"),
            "{Punctuation|<}{Name|ns:a}\n  {Attribute|x:y}{Punctuation|=}{String|\"1\"}\n\
             {Punctuation|>}"
        );
    }

    #[test]
    fn the_pieces_always_cover_the_whole_tag() {
        // A lost byte is the one failure a viewer cannot have. These are malformed on purpose.
        for markup in [
            "<",
            "</",
            "<a",
            "<a b",
            "<a b=",
            "<a b=\"x",
            "<a b=c d>",
            "<a / ? = >",
            "<a b=\"1\"c=\"2\">",
            "<é ü=\"ö\"/>",
            "<?",
            "",
        ] {
            let joined: String = lex_markup(markup.as_bytes())
                .into_iter()
                .map(|(r, _)| &markup[r])
                .collect();
            assert_eq!(joined, markup);
        }
    }

    // ---- the reader

    fn read_all(src: &Source) -> Vec<Event<'_>> {
        XmlReader::new()
            .read(src, &ReadContext::default())
            .unwrap()
            .map(|e| e.unwrap().node)
            .collect()
    }

    fn text_of(events: &[Event<'_>]) -> String {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Text(t) => Some(t.as_ref()),
                _ => None,
            })
            .collect()
    }

    fn warnings(events: &[Event<'_>]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Diagnostic(d) => Some(d.message.to_string()),
                _ => None,
            })
            .collect()
    }

    const TRICKY: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
        <!DOCTYPE note SYSTEM \"note.dtd\">\n\
        <!-- a comment\n   over two lines -->\n\
        <root xmlns:x=\"urn:x\" a=\"1>2\">\n\
          <x:item id='1'>Tom &amp; Jerry &lt;3</x:item>\n\
          <empty/>\n\
          <![CDATA[ raw <b>not a tag</b> ]]>\n\
          <?pi data?>\n\
        </root>\n";

    #[test]
    fn the_text_survives_byte_for_byte() {
        let src = Source::from_bytes("t.xml", TRICKY);
        let events = read_all(&src);
        assert_eq!(text_of(&events), TRICKY);
        assert!(warnings(&events).is_empty(), "{:?}", warnings(&events));
    }

    #[test]
    fn the_stream_is_balanced_and_borrowed() {
        let src = Source::from_bytes("t.xml", TRICKY);
        let events = read_all(&src);
        assert!(matches!(
            events.first(),
            Some(Event::Start(Tag::Document(_)))
        ));
        assert!(matches!(events.last(), Some(Event::End(TagKind::Document))));
        let opens = events
            .iter()
            .filter(|e| matches!(e, Event::Start(_)))
            .count();
        let closes = events.iter().filter(|e| matches!(e, Event::End(_))).count();
        assert_eq!(opens, closes);
        for e in &events {
            if let Event::Text(t) = e {
                assert!(matches!(t, Cow::Borrowed(_)), "'{t}' was copied");
            }
        }
    }

    #[test]
    fn comments_and_cdata_get_their_own_roles() {
        let src = Source::from_bytes("t.xml", TRICKY);
        let events = read_all(&src);
        let roles: Vec<TokenRole> = events
            .iter()
            .filter_map(|e| match e {
                Event::Start(Tag::Token { role }) => Some(*role),
                _ => None,
            })
            .collect();
        for wanted in [
            TokenRole::Comment,
            TokenRole::Name,
            TokenRole::Attribute,
            TokenRole::String,
            TokenRole::Punctuation,
        ] {
            assert!(roles.contains(&wanted), "no {wanted:?} in {roles:?}");
        }
    }

    #[test]
    fn a_long_comment_is_emitted_a_line_at_a_time() {
        // Otherwise one text node would be one event, and its size would become memory.
        let body = "line\n".repeat(50);
        let src = Source::from_bytes("t.xml", format!("<!--\n{body}-->\n"));
        let texts = read_all(&src)
            .into_iter()
            .filter(|e| matches!(e, Event::Text(_)))
            .count();
        assert!(texts >= 50, "{texts} text events");
    }

    #[test]
    fn mismatched_and_stray_end_tags_are_not_errors() {
        let input = "<a><b></a></c>\n";
        let src = Source::from_bytes("t.xml", input);
        let events = read_all(&src);
        assert_eq!(text_of(&events), input);
        assert!(warnings(&events).is_empty(), "{:?}", warnings(&events));
    }

    #[test]
    fn a_broken_document_warns_once_and_loses_nothing() {
        let input = "<a>ok</a>\n<b attr=\"never closed";
        let src = Source::from_bytes("t.xml", input);
        let events = read_all(&src);
        assert_eq!(text_of(&events), input);
        let w = warnings(&events);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("not well-formed"), "{}", w[0]);
    }

    #[test]
    fn an_empty_file_is_an_empty_document() {
        let src = Source::from_bytes("t.xml", "");
        let events = read_all(&src);
        assert_eq!(text_of(&events), "");
        assert!(matches!(events.last(), Some(Event::End(TagKind::Document))));
    }

    #[test]
    fn invalid_utf8_is_shown_with_one_warning() {
        let src = Source::from_bytes("t.xml", b"<a>\xFF</a>\n<b>\xFE</b>\n".to_vec());
        let events = read_all(&src);
        assert!(text_of(&events).contains('\u{FFFD}'));
        // One cause, one warning, and it names the cause rather than blaming the markup.
        let w = warnings(&events);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("not valid UTF-8"), "{}", w[0]);
    }

    #[test]
    fn a_declared_encoding_is_honored() {
        // The encoding is the detection layer's to resolve; the reader applies it. Latin-1
        // bytes must come out as the characters they are.
        let mut src = Source::from_bytes("t.xml", b"<a>Comit\xE9</a>\n".to_vec());
        src.set_encoding("windows-1252").unwrap();
        let events = read_all(&src);
        assert_eq!(text_of(&events), "<a>Comité</a>\n");
        assert!(warnings(&events).is_empty());
    }

    #[test]
    fn capabilities_claim_streaming_because_it_streams() {
        assert!(XmlReader::new().capabilities().streaming);
    }
}
