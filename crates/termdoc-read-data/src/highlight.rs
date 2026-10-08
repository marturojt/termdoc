//! What the line-by-line highlighters (YAML, TOML) have in common.
//!
//! Both read a source one line at a time, colour each line on its own with a little state
//! carried between lines, and emit the text byte for byte as borrowed slices. Only the colouring
//! differs, so that is the one thing a format supplies: a `lex` function from a line (without its
//! terminator) to coloured [`Piece`]s that cover it exactly.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::ops::Range;

use termdoc_core::{
    Diagnostic, Event, FormatId, Metadata, Result, Source, Span, Spanned, Tag, TagKind, TokenRole,
};

/// A coloured run of one line, as a byte range of its body. `None` is uncoloured.
pub(crate) type Piece = (Range<usize>, Option<TokenRole>);

/// Colours one line. `S` is whatever the format carries from one line to the next.
pub(crate) type Lex<S> = fn(&str, &mut S) -> Vec<Piece>;

/// The event stream of a highlighted document: `Document`, `Preformatted`, then each line as
/// plain `Text` and `Token` runs, each line keeping its terminator.
pub(crate) struct HighlightEvents<'a, S> {
    src: &'a Source,
    bytes: &'a [u8],
    pos: usize,
    line: u32,
    state: S,
    lex: Lex<S>,
    pending: VecDeque<Spanned<Event<'a>>>,
    warned_encoding: bool,
    done: bool,
}

impl<'a, S: Default> HighlightEvents<'a, S> {
    pub(crate) fn new(src: &'a Source, format: FormatId, lex: Lex<S>) -> Self {
        let mut pending = VecDeque::new();
        pending.push_back(Spanned::bare(Event::Start(Tag::Document(Box::new(
            Metadata {
                source_format: Some(format),
                ..Metadata::default()
            },
        )))));
        pending.push_back(Spanned::bare(Event::Start(Tag::Preformatted)));
        HighlightEvents {
            src,
            bytes: src.bytes(),
            pos: 0,
            line: 0,
            state: S::default(),
            lex,
            pending,
            warned_encoding: false,
            done: false,
        }
    }

    /// Reads one line and queues its events.
    fn fill(&mut self) {
        if self.pos >= self.bytes.len() {
            self.pending
                .push_back(Spanned::bare(Event::End(TagKind::Preformatted)));
            self.pending
                .push_back(Spanned::bare(Event::End(TagKind::Document)));
            self.done = true;
            return;
        }

        let start = self.pos;
        let rest = &self.bytes[start..];
        // The terminator stays on the line: inside `Preformatted` it is what closes it.
        let (raw, advance) = match rest.iter().position(|b| *b == b'\n') {
            Some(nl) => (&rest[..=nl], nl + 1),
            None => (rest, rest.len()),
        };
        self.pos += advance;
        self.line += 1;
        let span = Span::at_line(start as u64, self.pos as u64, self.line);

        let (line, had_errors) = self.src.decode_line(raw);
        if had_errors && !self.warned_encoding {
            // Once: a warning per line of a large file is worse than the problem.
            self.warned_encoding = true;
            self.pending.push_back(Spanned::new(
                Event::Diagnostic(Diagnostic::warning(format!(
                    "line {} is not valid {}; shown with replacements",
                    self.line,
                    self.src.encoding_name()
                ))),
                span,
            ));
        }

        let body_len = line.trim_end_matches(['\n', '\r']).len();
        let pieces = (self.lex)(&line[..body_len], &mut self.state);
        for (range, role) in pieces {
            let text = slice(&line, range.start, range.end);
            match role {
                Some(role) => {
                    self.pending
                        .push_back(Spanned::new(Event::Start(Tag::Token { role }), span));
                    self.pending
                        .push_back(Spanned::new(Event::Text(text), span));
                    self.pending
                        .push_back(Spanned::new(Event::End(TagKind::Token), span));
                }
                None => self
                    .pending
                    .push_back(Spanned::new(Event::Text(text), span)),
            }
        }
        if body_len < line.len() {
            self.pending.push_back(Spanned::new(
                Event::Text(slice(&line, body_len, line.len())),
                span,
            ));
        }
    }
}

impl<'a, S: Default> Iterator for HighlightEvents<'a, S> {
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

/// A sub-slice that stays borrowed when the whole is borrowed.
fn slice<'a>(text: &Cow<'a, str>, from: usize, to: usize) -> Cow<'a, str> {
    match text {
        Cow::Borrowed(s) => Cow::Borrowed(&s[from..to]),
        Cow::Owned(s) => Cow::Owned(s[from..to].to_string()),
    }
}

/// Accumulates coloured runs and fills the gaps between them with uncoloured ones, so the
/// pieces always cover the whole line.
pub(crate) struct Painter {
    pieces: Vec<Piece>,
    at: usize,
}

impl Painter {
    pub(crate) fn new() -> Self {
        Painter {
            pieces: Vec::new(),
            at: 0,
        }
    }

    /// Colours `from..to`. Empty ranges, and ranges behind what is already painted, are
    /// ignored: a colouring mistake must never be able to lose or repeat text.
    pub(crate) fn tok(&mut self, from: usize, to: usize, role: TokenRole) {
        if from >= to || from < self.at {
            return;
        }
        if from > self.at {
            self.pieces.push((self.at..from, None));
        }
        self.pieces.push((from..to, Some(role)));
        self.at = to;
    }

    pub(crate) fn finish(mut self, len: usize) -> Vec<Piece> {
        if self.at < len {
            self.pieces.push((self.at..len, None));
        }
        self.pieces
    }
}

pub(crate) fn skip_spaces(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    i
}

/// `end` moved back over trailing spaces, but never before `from`.
pub(crate) fn trim_end(b: &[u8], from: usize, mut end: usize) -> usize {
    while end > from && (b[end - 1] == b' ' || b[end - 1] == b'\t') {
        end -= 1;
    }
    end
}
