//! The JSON reader.
//!
//! # Why this one materialises, and why that is bounded
//!
//! `serde_json` builds a `Value` tree, which is the opposite of the streaming model the rest
//! of the pipeline is built on. That was a deliberate trade — a hand-written streaming
//! tokenizer duplicates a decade of `serde_json`'s work on escapes, surrogate pairs and
//! number edge cases — but it is only defensible with a ceiling on it.
//!
//! **Measured end to end** (Apple silicon, release build, `/usr/bin/time -l` peak footprint,
//! scalar-heavy input — the worst shape, since every small number is its own node):
//!
//! | input | 2026-08-11, pretty-printed `String` | 2026-10-08, lazy token walk |
//! |---|---|---|
//! | 128 KB | 6.7 MB (~54x) | 3.1 MB (~24x) |
//! | 256 KB | 12.1 MB (~48x) | 6.6 MB (~26x) |
//! | 512 KB | 22.6 MB (~45x) | 9.5 MB (~19x) |
//!
//! The drop is the pretty-printed `String` and the single event carrying it, both gone: the
//! value is now walked lazily and a few events are queued per step.
//!
//! **Measure the whole pipeline, not the parser.** `serde_json::from_slice` alone accounts for
//! only ~12x; a ceiling derived from the parser in isolation came out four times too generous,
//! and the error was invisible until the binary itself was put under `/usr/bin/time -l`.
//!
//! The process budget is 50 MB of anonymous memory (docs/DESIGN.md §8). [`MAX_MATERIALISED`]
//! is 512 KiB, a figure set when the cost was ~45x. At today's ~19-26x it leaves roughly
//! twice the headroom it was chosen for; raising it is a decision to take with a measurement,
//! not a free win. Half a megabyte of JSON is already some fifteen thousand pretty-printed
//! lines — well past what anyone reads rather than greps.
//!
//! Above the threshold the document is neither refused nor truncated: it is emitted verbatim,
//! lazily, with a [`Diagnostic`] explaining why it is unformatted. The reader makes that call
//! rather than `run.rs`, because `pick_reader` decides by *format* and has no business knowing
//! about byte counts.
//!
//! # A limitation this does not fix
//!
//! Memory on the verbatim path is proportional to the longest **line**, not to the file. A
//! 16 MB minified document — one line — costs about 68 MB; the same 16 MB with newlines in it
//! costs 1.2 MB. That is the layout engine holding one line at a time, which is ordinarily the
//! point, and it is exactly the shape a large minified JSON has. Splitting the line here would
//! mean inventing content, so it is recorded rather than papered over.
//!
//! # Styling
//!
//! Keys, strings, numbers, booleans, `null` and punctuation are each wrapped in a
//! [`Tag::Token`] with a [`TokenRole`]. The reader says what the text *is*; the theme decides
//! how it looks. Several tokens share one line inside `Preformatted` because the layout
//! accumulates text until a newline closes the line (docs/DESIGN.md §2.2). The verbatim path
//! carries no tokens: it is not parsed, so it has no roles to report.

use std::borrow::Cow;
use std::collections::VecDeque;

use serde_json::Value;
use termdoc_core::{
    Diagnostic, DocumentReader, Event, Events, FormatId, Metadata, ReadContext, ReaderCaps, Result,
    Source, Spanned, Tag, TagKind, TokenRole,
};

/// The largest input that gets parsed into a `Value`. See the module header for the
/// measurement this comes from.
pub const MAX_MATERIALISED: usize = 512 * 1024;

#[derive(Debug, Clone, Copy, Default)]
pub struct JsonReader;

impl JsonReader {
    pub fn new() -> Self {
        JsonReader
    }
}

impl DocumentReader for JsonReader {
    fn id(&self) -> FormatId {
        FormatId::Json
    }

    fn capabilities(&self) -> ReaderCaps {
        ReaderCaps {
            // Honestly false. `serde_json` materialises, and nothing in the codebase reads
            // this field yet, which is exactly why an aspirational `true` would rot here
            // with no test to catch it.
            streaming: false,
            paginated: false,
            metadata: false,
        }
    }

    fn read<'a>(&self, src: &'a Source, _ctx: &ReadContext) -> Result<Events<'a>> {
        let mut events: Vec<Spanned<Event<'a>>> = Vec::with_capacity(6);
        events.push(Spanned::bare(Event::Start(Tag::Document(Box::new(
            Metadata {
                source_format: Some(FormatId::Json),
                ..Metadata::default()
            },
        )))));

        let formatted = if src.len() > MAX_MATERIALISED {
            events.push(Spanned::bare(Event::Diagnostic(Diagnostic::warning(
                format!(
                    "{} is larger than the {} KiB limit for structured rendering; \
                     showing it unformatted",
                    src.display_name(),
                    MAX_MATERIALISED / 1024
                ),
            ))));
            None
        } else {
            match serde_json::from_slice::<serde_json::Value>(src.bytes()) {
                Ok(value) => Some(value),
                Err(e) => {
                    // Partial rendering beats total failure: say what is wrong on stderr,
                    // then show the bytes so it can be seen for what it is.
                    events.push(Spanned::bare(Event::Diagnostic(Diagnostic::warning(
                        format!("not valid JSON ({e}); showing it unformatted"),
                    ))));
                    None
                }
            }
        };

        events.push(Spanned::bare(Event::Start(Tag::Preformatted)));

        match formatted {
            // The value is walked lazily rather than rendered to a string first. Measured, the
            // pretty-printed `String` was a large part of the ~45x cost, and a walk holds only
            // the tree and a stack of open containers.
            Some(value) => Ok(Box::new(
                events
                    .into_iter()
                    .chain(PrettyEvents::new(value))
                    .chain(std::iter::once(Spanned::bare(Event::End(
                        TagKind::Preformatted,
                    ))))
                    .chain(std::iter::once(Spanned::bare(Event::End(
                        TagKind::Document,
                    ))))
                    .map(Ok),
            )),
            // The fallback must not materialise, or the size ceiling would be pointless: the
            // whole reason for refusing to parse a 500 MB document is not spending 500 MB on
            // it, and reading it into a `String` to hand over as one event spends it anyway.
            // Measured before this was fixed: a 16 MB input cost 117 MB. Lines are walked
            // lazily and borrowed from the source instead, exactly as the plain-text reader
            // does.
            None => Ok(Box::new(
                events
                    .into_iter()
                    .chain(RawLines::new(src))
                    .chain(std::iter::once(Spanned::bare(Event::End(
                        TagKind::Preformatted,
                    ))))
                    .chain(std::iter::once(Spanned::bare(Event::End(
                        TagKind::Document,
                    ))))
                    .map(Ok),
            )),
        }
    }
}

/// One open container, with the children it has yet to emit.
enum Frame {
    Array(std::vec::IntoIter<Value>),
    Object(serde_json::map::IntoIter),
}

/// Walks a `Value` and emits it the way `serde_json::to_string_pretty` would print it —
/// two-space indent, `"key": value` — with every key, scalar and bracket wrapped in a
/// [`Tag::Token`] so the theme can tell them apart.
///
/// Lazy: a handful of events are queued per step, so memory is the tree plus the stack.
struct PrettyEvents<'a> {
    root: Option<Value>,
    stack: Vec<Frame>,
    /// Whether the container on top of the stack has emitted an item yet.
    first: Vec<bool>,
    pending: VecDeque<Event<'a>>,
}

impl<'a> PrettyEvents<'a> {
    fn new(value: Value) -> Self {
        PrettyEvents {
            root: Some(value),
            stack: Vec::new(),
            first: Vec::new(),
            pending: VecDeque::new(),
        }
    }

    fn token(&mut self, role: TokenRole, text: impl Into<Cow<'a, str>>) {
        self.pending.push_back(Event::Start(Tag::Token { role }));
        self.pending.push_back(Event::Text(text.into()));
        self.pending.push_back(Event::End(TagKind::Token));
    }

    fn indent(&mut self, depth: usize) {
        const SPACES: &str = "                                                                ";
        let width = depth * 2;
        if width <= SPACES.len() {
            self.pending
                .push_back(Event::Text(Cow::Borrowed(&SPACES[..width])));
        } else {
            self.pending
                .push_back(Event::Text(Cow::Owned(" ".repeat(width))));
        }
    }

    /// Emits a value that sits at the current depth. A container is opened and left on the
    /// stack; a scalar is complete.
    fn begin(&mut self, value: Value) {
        match value {
            Value::Null => self.token(TokenRole::Null, "null"),
            Value::Bool(b) => self.token(TokenRole::Bool, if b { "true" } else { "false" }),
            Value::Number(n) => self.token(TokenRole::Number, n.to_string()),
            Value::String(s) => self.token(TokenRole::String, quote(&s)),
            Value::Array(items) if items.is_empty() => self.token(TokenRole::Punctuation, "[]"),
            Value::Object(map) if map.is_empty() => self.token(TokenRole::Punctuation, "{}"),
            Value::Array(items) => {
                self.token(TokenRole::Punctuation, "[");
                self.stack.push(Frame::Array(items.into_iter()));
                self.first.push(true);
            }
            Value::Object(map) => {
                self.token(TokenRole::Punctuation, "{");
                self.stack.push(Frame::Object(map.into_iter()));
                self.first.push(true);
            }
        }
    }

    /// Queues the next few events. Returns `false` when the document is finished.
    fn advance(&mut self) -> bool {
        if let Some(value) = self.root.take() {
            self.begin(value);
            return true;
        }
        let depth = self.stack.len();
        let Some(top) = self.stack.last_mut() else {
            return false;
        };
        let (item, closer) = match top {
            Frame::Array(it) => (it.next().map(|v| (None, v)), "]"),
            Frame::Object(it) => (it.next().map(|(k, v)| (Some(k), v)), "}"),
        };
        match item {
            Some((key, value)) => {
                let first = std::mem::replace(self.first.last_mut().expect("paired"), false);
                if !first {
                    self.token(TokenRole::Punctuation, ",");
                }
                self.pending.push_back(Event::Text(Cow::Borrowed("\n")));
                self.indent(depth);
                if let Some(key) = key {
                    self.token(TokenRole::Key, quote(&key));
                    self.token(TokenRole::Punctuation, ": ");
                }
                self.begin(value);
            }
            None => {
                self.stack.pop();
                self.first.pop();
                self.pending.push_back(Event::Text(Cow::Borrowed("\n")));
                self.indent(depth - 1);
                self.token(TokenRole::Punctuation, closer);
            }
        }
        true
    }
}

impl<'a> Iterator for PrettyEvents<'a> {
    type Item = Spanned<Event<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.pending.is_empty() {
            if !self.advance() {
                return None;
            }
        }
        self.pending.pop_front().map(Spanned::bare)
    }
}

/// A JSON string literal, escaped exactly as `serde_json` escapes it.
fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""))
}

/// Walks the source line by line, borrowing each one.
///
/// This is the unformatted path, and it exists so that declining to parse a huge document
/// actually costs nothing. Decoding goes through the `Source`, because the encoding was
/// resolved by the detection layer and a reader that decides it again is how a latin-1 file
/// ends up full of replacement characters.
struct RawLines<'a> {
    src: &'a Source,
    bytes: &'a [u8],
    pos: usize,
    done: bool,
}

impl<'a> RawLines<'a> {
    fn new(src: &'a Source) -> Self {
        RawLines {
            src,
            bytes: src.bytes(),
            pos: 0,
            done: false,
        }
    }
}

impl<'a> Iterator for RawLines<'a> {
    type Item = Spanned<Event<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.pos >= self.bytes.len() {
            self.done = true;
            return None;
        }
        let rest = &self.bytes[self.pos..];
        let end = rest.iter().position(|b| *b == b'\n');
        // The terminator stays on the line: it is what closes a line inside `Preformatted`,
        // and the layout trims it.
        let (line, advance) = match end {
            Some(i) => (&rest[..=i], i + 1),
            None => (rest, rest.len()),
        };
        self.pos += advance;
        let (text, _) = self.src.decode_line(line);
        Some(Spanned::bare(Event::Text(text)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(bytes: &str) -> String {
        let src = Source::from_bytes("t.json", bytes.as_bytes().to_vec());
        let reader = JsonReader::new();
        let events = reader.read(&src, &ReadContext::default()).unwrap();
        events
            .filter_map(|e| match e.unwrap().node {
                Event::Text(t) => Some(t.into_owned()),
                _ => None,
            })
            .collect()
    }

    fn diagnostics(bytes: &[u8]) -> Vec<String> {
        let src = Source::from_bytes("t.json", bytes.to_vec());
        let reader = JsonReader::new();
        reader
            .read(&src, &ReadContext::default())
            .unwrap()
            .filter_map(|e| match e.unwrap().node {
                Event::Diagnostic(d) => Some(d.message.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn minified_input_comes_out_indented() {
        // The whole point of the reader: one unreadable line in, a tree out.
        let out = render(r#"{"a":1,"b":[2,3]}"#);
        assert_eq!(out, "{\n  \"a\": 1,\n  \"b\": [\n    2,\n    3\n  ]\n}");
    }

    #[test]
    fn invalid_json_is_shown_rather_than_refused() {
        // Partial rendering beats total failure — the bytes still reach the terminal.
        let broken = b"{\"a\": oops}";
        assert_eq!(
            render(std::str::from_utf8(broken).unwrap()),
            "{\"a\": oops}"
        );
        let diags = diagnostics(broken);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].contains("not valid JSON"), "got: {}", diags[0]);
    }

    #[test]
    fn oversized_input_is_not_materialised() {
        // Above the ceiling the reader must not build a Value at all: it warns and passes
        // the bytes straight through. The content is deliberately valid JSON, so anything
        // other than the size check would have formatted it.
        let big = format!("[{}0]", "0,".repeat(MAX_MATERIALISED / 2));
        assert!(big.len() > MAX_MATERIALISED);
        let diags = diagnostics(big.as_bytes());
        assert_eq!(diags.len(), 1);
        assert!(diags[0].contains("larger than"), "got: {}", diags[0]);
        assert_eq!(
            render(&big),
            big,
            "oversized input must pass through verbatim"
        );
    }

    #[test]
    fn capabilities_do_not_claim_streaming() {
        // `serde_json` materialises. Nothing reads this field yet, which is exactly why an
        // aspirational `true` would go unnoticed.
        assert!(!JsonReader::new().capabilities().streaming);
    }

    #[test]
    fn the_stream_is_well_formed() {
        let src = Source::from_bytes("t.json", b"{}".to_vec());
        let events: Vec<_> = JsonReader::new()
            .read(&src, &ReadContext::default())
            .unwrap()
            .map(|e| e.unwrap().node)
            .collect();
        assert!(matches!(
            events.first(),
            Some(Event::Start(Tag::Document(_)))
        ));
        assert!(matches!(events.last(), Some(Event::End(TagKind::Document))));
    }
}
