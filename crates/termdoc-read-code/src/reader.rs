//! The source-file reader: a file in a known language, highlighted.
//!
//! It emits `Preformatted` with `Token` runs rather than `CodeBlock`: `CodeBlock` carries the
//! theme's flat colour for code nobody could highlight, and a highlighted file must not be
//! painted yellow underneath its tokens. The same choice is made by the fenced-block transform
//! (`transform.rs`), so highlighted code looks the same wherever it comes from.
//!
//! Streaming, like the other line-by-line readers: a line is lexed when the layout asks for it,
//! so `termdoc big.rs | head` parses a few lines, and every piece borrows from the source.

use termdoc_core::highlight::{HighlightEvents, Piece};
use termdoc_core::{DocumentReader, Events, FormatId, ReadContext, ReaderCaps, Result, Source};

use crate::engine::{self, Lexer};

#[derive(Debug, Clone, Copy, Default)]
pub struct CodeReader;

impl CodeReader {
    pub fn new() -> Self {
        CodeReader
    }
}

impl DocumentReader for CodeReader {
    fn id(&self) -> FormatId {
        FormatId::SourceCode
    }

    fn capabilities(&self) -> ReaderCaps {
        ReaderCaps {
            // True: lines are lexed on demand and borrowed from the source.
            streaming: true,
            paginated: false,
            metadata: false,
        }
    }

    fn read<'a>(&self, src: &'a Source, ctx: &ReadContext) -> Result<Events<'a>> {
        let lexer = if !ctx.styled {
            // Nothing could show the colours, so the grammars are not even loaded.
            Lexer::plain(None)
        } else {
            match engine::syntax_for_source(src) {
                Some(syntax) => Lexer::new(syntax),
                None => Lexer::plain(Some(format!(
                    "no grammar is known for {}; shown without highlighting",
                    src.display_name()
                ))),
            }
        };
        Ok(Box::new(
            HighlightEvents::new(src, FormatId::SourceCode, lexer, lex).with_notice(notice),
        ))
    }
}

fn lex(line: &str, lexer: &mut Lexer) -> Vec<Piece> {
    lexer.lex(line)
}

fn notice(lexer: &mut Lexer) -> Option<String> {
    lexer.take_notice()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use termdoc_core::{Event, Tag, TagKind};

    /// A real file, since the grammar is found by the file's name. Each call gets its own
    /// directory: tests run in parallel.
    fn file(name: &str, content: &str) -> Source {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let dir: PathBuf = std::env::temp_dir().join(format!(
            "termdoc-code-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        Source::open(&path).unwrap()
    }

    fn events(src: Source, ctx: &ReadContext) -> Vec<Event<'static>> {
        // Leaked on purpose: the events borrow the source, and a test does not care.
        let src: &'static Source = Box::leak(Box::new(src));
        CodeReader::new()
            .read(src, ctx)
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

    const RUST: &str = "// hello\nfn main() {\n    let x: i32 = 42;\n    println!(\"{x}\");\n}\n";

    #[test]
    fn the_text_survives_byte_for_byte() {
        let src = file("a.rs", RUST);
        let ev = events(src, &ReadContext::default());
        assert_eq!(text_of(&ev), RUST);
        assert!(warnings(&ev).is_empty(), "{:?}", warnings(&ev));
    }

    #[test]
    fn a_known_language_is_highlighted() {
        let src = file("a.rs", RUST);
        let ev = events(src, &ReadContext::default());
        let roles: Vec<_> = ev
            .iter()
            .filter_map(|e| match e {
                Event::Start(Tag::Token { role }) => Some(*role),
                _ => None,
            })
            .collect();
        for wanted in [
            termdoc_core::TokenRole::Keyword,
            termdoc_core::TokenRole::Function,
            termdoc_core::TokenRole::Comment,
            termdoc_core::TokenRole::Number,
            termdoc_core::TokenRole::String,
        ] {
            assert!(roles.contains(&wanted), "no {wanted:?} in {roles:?}");
        }
    }

    #[test]
    fn it_is_preformatted_not_a_code_block() {
        // `CodeBlock` carries the theme's flat colour, which must not sit under the tokens.
        let src = file("a.rs", RUST);
        let ev = events(src, &ReadContext::default());
        assert!(
            ev.iter()
                .any(|e| matches!(e, Event::Start(Tag::Preformatted)))
        );
        assert!(
            !ev.iter()
                .any(|e| matches!(e, Event::Start(Tag::CodeBlock { .. })))
        );
    }

    #[test]
    fn the_stream_is_balanced_and_borrowed() {
        let src = file("a.rs", RUST);
        let ev = events(src, &ReadContext::default());
        let opens = ev.iter().filter(|e| matches!(e, Event::Start(_))).count();
        let closes = ev.iter().filter(|e| matches!(e, Event::End(_))).count();
        assert_eq!(opens, closes);
        assert!(matches!(ev.last(), Some(Event::End(TagKind::Document))));
        for e in &ev {
            if let Event::Text(t) = e {
                assert!(matches!(t, Cow::Borrowed(_)), "'{t}' was copied");
            }
        }
    }

    #[test]
    fn output_that_cannot_show_colour_is_not_parsed() {
        let src = file("a.rs", RUST);
        let ctx = ReadContext {
            styled: false,
            ..ReadContext::default()
        };
        let ev = events(src, &ctx);
        assert_eq!(text_of(&ev), RUST);
        assert!(
            !ev.iter()
                .any(|e| matches!(e, Event::Start(Tag::Token { .. })))
        );
        assert!(warnings(&ev).is_empty());
    }

    #[test]
    fn an_unknown_language_is_shown_plain_with_one_warning() {
        let src = file("notes.zzqq", "fn main() {}\nsecond line\n");
        let ev = events(src, &ReadContext::default());
        assert_eq!(text_of(&ev), "fn main() {}\nsecond line\n");
        let w = warnings(&ev);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("no grammar"), "{}", w[0]);
    }

    #[test]
    fn a_shebang_is_enough_without_an_extension() {
        let src = file("run", "#!/usr/bin/env python3\nprint('hi')\n");
        let ev = events(src, &ReadContext::default());
        assert!(
            ev.iter()
                .any(|e| matches!(e, Event::Start(Tag::Token { .. })))
        );
        assert!(warnings(&ev).is_empty());
    }

    #[test]
    fn crlf_files_lose_nothing() {
        let src = file("w.rs", "fn a() {}\r\nfn b() {}\r\n");
        let ev = events(src, &ReadContext::default());
        assert_eq!(text_of(&ev), "fn a() {}\r\nfn b() {}\r\n");
    }

    #[test]
    fn capabilities_claim_streaming_because_lines_are_lexed_on_demand() {
        assert!(CodeReader::new().capabilities().streaming);
    }
}
