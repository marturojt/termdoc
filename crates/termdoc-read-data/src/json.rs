//! The JSON reader.
//!
//! # Why this one materialises, and why that is bounded
//!
//! `serde_json` builds a `Value` tree, which is the opposite of the streaming model the rest
//! of the pipeline is built on. That was a deliberate trade — a hand-written streaming
//! tokenizer duplicates a decade of `serde_json`'s work on escapes, surrogate pairs and
//! number edge cases — but it is only defensible with a ceiling on it.
//!
//! **Measured end to end on 2026-08-11** (Apple silicon, release build, scalar-heavy input —
//! the worst shape, since every small number is its own node):
//!
//! | input | process own memory | ratio |
//! |---|---|---|
//! | 128 KB | 6.7 MB | ~54x |
//! | 256 KB | 12.1 MB | ~48x |
//! | 512 KB | 22.6 MB | ~45x |
//!
//! **Measure the whole pipeline, not the parser.** `serde_json::from_slice` alone accounts for
//! only ~12x; the rest is the pretty-printed `String` and the event carrying it. A ceiling
//! derived from the parser in isolation came out four times too generous, and the error was
//! invisible until the binary itself was put under `/usr/bin/time -l`.
//!
//! The process budget is 50 MB of anonymous memory (docs/DESIGN.md §8). [`MAX_MATERIALISED`]
//! is therefore 512 KiB, which costs about 23 MB at the worst observed ratio and leaves the
//! rest of the pipeline room. Half a megabyte of JSON is already some fifteen thousand
//! pretty-printed lines — well past what anyone reads rather than greps.
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
//! # Why the output carries no per-token styling
//!
//! Inside [`Tag::Preformatted`] the layout engine emits **one line per `Text` event** and
//! takes the style from the enclosing block, so a key and its value cannot carry different
//! styles without being different lines. Marking keys with an inline tag produced exactly
//! that: every key on a line of its own. Distinguishing keys from scalars therefore needs
//! either a data-role concept in the document model or segment-level styling inside
//! preformatted blocks, and both are layout or model changes rather than reader changes.
//!
//! What this reader delivers instead is the thing that actually makes JSON readable in a
//! terminal: structure. A minified document arrives as one 40 KB line and leaves as an
//! indented tree.

use termdoc_core::{
    Diagnostic, DocumentReader, Event, Events, FormatId, Metadata, ReadContext, ReaderCaps, Result,
    Source, Spanned, Tag, TagKind,
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
                Ok(value) => serde_json::to_string_pretty(&value).ok(),
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
            // The engine splits preformatted text on newlines itself, so the whole document
            // is one event. That also keeps `serde_json`'s pretty printer as the single
            // authority on indentation and escaping rather than reimplementing it here.
            Some(text) => {
                events.push(Spanned::bare(Event::Text(text.into())));
                events.push(Spanned::bare(Event::End(TagKind::Preformatted)));
                events.push(Spanned::bare(Event::End(TagKind::Document)));
                Ok(Box::new(events.into_iter().map(Ok)))
            }
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
        let (line, advance) = match end {
            Some(i) => (&rest[..i], i + 1),
            None => (rest, rest.len()),
        };
        self.pos += advance;
        let line = line.strip_suffix(b"\r").unwrap_or(line);
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
