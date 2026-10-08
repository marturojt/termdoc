//! What the line-by-line highlighters (YAML, TOML, source code) have in common.
//!
//! They read a source one line at a time, colour each line on its own with a little state
//! carried between lines, and emit the text byte for byte as borrowed slices. Only the colouring
//! differs, so that is the one thing a format supplies: a `lex` function from a line (without its
//! terminator) to coloured [`Piece`]s that cover it exactly.
//!
//! It lives in `core`, next to [`Source::decode_line`], because a reader may depend on `core`
//! and nothing else, and this is reader plumbing rather than any one format's business.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::ops::Range;

use crate::stream::LineStream;
use crate::{
    Diagnostic, Error, Event, FormatId, Metadata, Result, Source, Span, Spanned, Tag, TagKind,
    TokenRole,
};

/// A coloured run of one line, as a byte range of its body. `None` is uncoloured.
pub type Piece = (Range<usize>, Option<TokenRole>);

/// Colours one line. `S` is whatever the format carries from one line to the next.
pub type Lex<S> = fn(&str, &mut S) -> Vec<Piece>;

/// Asked after every line: has the colouring something to tell the user? Returned once, as a
/// warning in the stream.
pub type Notice<S> = fn(&mut S) -> Option<String>;

/// Where the lines come from: a source that is all there, or an input still arriving.
enum Input<'a> {
    Source {
        src: &'a Source,
        bytes: &'a [u8],
        pos: usize,
    },
    Stream(LineStream),
}

/// One line, however it arrived.
struct Fetched<'a> {
    /// With its terminator, if it had one.
    text: Cow<'a, str>,
    had_errors: bool,
    start: u64,
    end: u64,
    encoding: &'static str,
}

/// The event stream of a highlighted document: `Document`, `Preformatted`, then each line as
/// plain `Text` and `Token` runs, each line keeping its terminator.
pub struct HighlightEvents<'a, S> {
    input: Input<'a>,
    line: u32,
    state: S,
    lex: Lex<S>,
    notice: Option<Notice<S>>,
    pending: VecDeque<Spanned<Event<'a>>>,
    /// An I/O error that ended a stream, delivered once the events before it have been.
    error: Option<Error>,
    warned_encoding: bool,
    done: bool,
}

impl<S> std::fmt::Debug for HighlightEvents<'_, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HighlightEvents")
            .field("line", &self.line)
            .finish_non_exhaustive()
    }
}

impl<'a, S> HighlightEvents<'a, S> {
    pub fn new(src: &'a Source, format: FormatId, state: S, lex: Lex<S>) -> Self {
        Self::with_input(
            Input::Source {
                src,
                bytes: src.bytes(),
                pos: 0,
            },
            format,
            state,
            lex,
        )
    }

    fn with_input(input: Input<'a>, format: FormatId, state: S, lex: Lex<S>) -> Self {
        let mut pending = VecDeque::new();
        pending.push_back(Spanned::bare(Event::Start(Tag::Document(Box::new(
            Metadata {
                source_format: Some(format),
                ..Metadata::default()
            },
        )))));
        pending.push_back(Spanned::bare(Event::Start(Tag::Preformatted)));
        HighlightEvents {
            input,
            line: 0,
            state,
            lex,
            notice: None,
            pending,
            error: None,
            warned_encoding: false,
            done: false,
        }
    }

    /// Adds a hook that can raise a warning after any line, for a colouring that gives up
    /// partway (a size ceiling, say) and should say so.
    pub fn with_notice(mut self, notice: Notice<S>) -> Self {
        self.notice = Some(notice);
        self
    }

    /// The next line, waiting for it if it is still arriving.
    fn next_line(&mut self) -> Option<Fetched<'a>> {
        match &mut self.input {
            Input::Source { src, bytes, pos } => {
                if *pos >= bytes.len() {
                    return None;
                }
                let start = *pos;
                let rest = &bytes[start..];
                // The terminator stays on the line: inside `Preformatted` it is what closes it.
                let (raw, advance) = match rest.iter().position(|b| *b == b'\n') {
                    Some(nl) => (&rest[..=nl], nl + 1),
                    None => (rest, rest.len()),
                };
                *pos += advance;
                let (text, had_errors) = src.decode_line(raw);
                Some(Fetched {
                    text,
                    had_errors,
                    start: start as u64,
                    end: *pos as u64,
                    encoding: src.encoding_name(),
                })
            }
            Input::Stream(stream) => match stream.next_line() {
                Some(line) => Some(Fetched {
                    text: Cow::Owned(line.text),
                    had_errors: line.had_errors,
                    start: line.start,
                    end: line.end,
                    encoding: stream.encoding_name(),
                }),
                None => {
                    if let Some(e) = stream.take_error() {
                        self.error = Some(Error::from(e));
                    }
                    None
                }
            },
        }
    }

    /// Reads one line and queues its events.
    fn fill(&mut self) {
        let Some(fetched) = self.next_line() else {
            if self.error.is_none() {
                self.pending
                    .push_back(Spanned::bare(Event::End(TagKind::Preformatted)));
                self.pending
                    .push_back(Spanned::bare(Event::End(TagKind::Document)));
            }
            self.done = true;
            return;
        };
        self.line += 1;
        let span = Span::at_line(fetched.start, fetched.end, self.line);
        let Fetched {
            text: line,
            had_errors,
            encoding,
            ..
        } = fetched;

        if had_errors && !self.warned_encoding {
            // Once: a warning per line of a large file is worse than the problem.
            self.warned_encoding = true;
            self.pending.push_back(Spanned::new(
                Event::Diagnostic(Diagnostic::warning(format!(
                    "line {} is not valid {encoding}; shown with replacements",
                    self.line,
                ))),
                span,
            ));
        }

        let body_len = line.trim_end_matches(['\n', '\r']).len();
        let pieces = (self.lex)(&line[..body_len], &mut self.state);

        // A line with nothing coloured in it is one `Text`, terminator and all: two events
        // for every line of a plain log would be a large cost for no information.
        if let [(range, None)] = pieces.as_slice()
            && range.start == 0
            && range.end == body_len
        {
            self.pending
                .push_back(Spanned::new(Event::Text(line), span));
        } else {
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
        if let Some(notice) = self.notice
            && let Some(message) = notice(&mut self.state)
        {
            self.pending.push_back(Spanned::new(
                Event::Diagnostic(Diagnostic::warning(message)),
                span,
            ));
        }
    }
}

impl<'a, S> HighlightEvents<'a, S> {
    /// Highlights an input that is still arriving, a line at a time, as each line does.
    /// Nothing already shown is kept, so a stream that runs for a week costs what a minute does.
    pub fn from_stream(stream: LineStream, format: FormatId, state: S, lex: Lex<S>) -> Self {
        Self::with_input(Input::Stream(stream), format, state, lex)
    }
}

impl<'a, S> Iterator for HighlightEvents<'a, S> {
    type Item = Result<Spanned<Event<'a>>>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(Ok(event));
            }
            if let Some(e) = self.error.take() {
                return Some(Err(e));
            }
            if self.done {
                return None;
            }
            self.fill();
        }
    }
}

/// A sub-slice that stays borrowed when the whole is borrowed.
pub fn slice<'a>(text: &Cow<'a, str>, from: usize, to: usize) -> Cow<'a, str> {
    match text {
        Cow::Borrowed(s) => Cow::Borrowed(&s[from..to]),
        Cow::Owned(s) => Cow::Owned(s[from..to].to_string()),
    }
}

/// Accumulates coloured runs and fills the gaps between them with uncoloured ones, so the
/// pieces always cover the whole line.
#[derive(Debug, Default)]
pub struct Painter {
    pieces: Vec<Piece>,
    at: usize,
}

impl Painter {
    pub fn new() -> Self {
        Painter {
            pieces: Vec::new(),
            at: 0,
        }
    }

    /// Colours `from..to`. Empty ranges, and ranges behind what is already painted, are
    /// ignored: a colouring mistake must never be able to lose or repeat text.
    pub fn tok(&mut self, from: usize, to: usize, role: TokenRole) {
        if from >= to || from < self.at {
            return;
        }
        if from > self.at {
            self.pieces.push((self.at..from, None));
        }
        self.pieces.push((from..to, Some(role)));
        self.at = to;
    }

    pub fn finish(mut self, len: usize) -> Vec<Piece> {
        if self.at < len {
            self.pieces.push((self.at..len, None));
        }
        self.pieces
    }
}

pub fn skip_spaces(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    i
}

/// `end` moved back over trailing spaces, but never before `from`.
pub fn trim_end(b: &[u8], from: usize, mut end: usize) -> usize {
    while end > from && (b[end - 1] == b' ' || b[end - 1] == b'\t') {
        end -= 1;
    }
    end
}
