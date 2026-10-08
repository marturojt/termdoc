//! Highlights fenced code blocks in any document.
//!
//! A `CodeBlock` whose language has a grammar is rewritten as `Preformatted` with `Token` runs
//! in it; one whose language is unknown, or absent, or `text`, goes through untouched and keeps
//! the theme's flat code colour. So the stream a reader produced is never wrong, only
//! sometimes richer.
//!
//! Streaming: lines are lexed as the `Text` events arrive. A chunk that stops mid-line is
//! carried until its newline comes, and a chunk made of whole lines is sliced without copying.

use std::borrow::Cow;
use std::collections::VecDeque;

use termdoc_core::highlight::slice;
use termdoc_core::{Diagnostic, Event, Events, Result, Span, Spanned, Tag, TagKind, Transform};

use crate::engine::{self, Lexer};

#[derive(Debug, Clone, Copy, Default)]
pub struct CodeBlockHighlighter;

impl CodeBlockHighlighter {
    pub fn new() -> Self {
        CodeBlockHighlighter
    }
}

impl Transform for CodeBlockHighlighter {
    fn apply<'a>(&self, events: Events<'a>) -> Events<'a> {
        Box::new(Blocks {
            inner: events,
            pending: VecDeque::new(),
            block: None,
        })
    }
}

/// The block being highlighted.
struct Block {
    lexer: Lexer,
    /// The start of a line whose end has not arrived.
    carry: String,
}

struct Blocks<'a> {
    inner: Events<'a>,
    pending: VecDeque<Result<Spanned<Event<'a>>>>,
    block: Option<Block>,
}

impl<'a> Blocks<'a> {
    fn push(&mut self, event: Event<'a>, span: Span) {
        self.pending.push_back(Ok(Spanned::new(event, span)));
    }

    /// Lexes one complete line (terminator included, if it has one) and queues its events.
    fn line(&mut self, line: Cow<'a, str>, span: Span) {
        let body_len = line.trim_end_matches(['\n', '\r']).len();
        let (pieces, notice) = {
            let block = self.block.as_mut().expect("inside a highlighted block");
            let pieces = block.lexer.lex(&line[..body_len]);
            (pieces, block.lexer.take_notice())
        };
        for (range, role) in pieces {
            let text = slice(&line, range.start, range.end);
            match role {
                Some(role) => {
                    self.push(Event::Start(Tag::Token { role }), span);
                    self.push(Event::Text(text), span);
                    self.push(Event::End(TagKind::Token), span);
                }
                None => self.push(Event::Text(text), span),
            }
        }
        if body_len < line.len() {
            self.push(Event::Text(slice(&line, body_len, line.len())), span);
        }
        if let Some(message) = notice {
            self.push(Event::Diagnostic(Diagnostic::warning(message)), span);
        }
    }

    /// One piece of text that ends at a newline, or at the end of the chunk.
    fn piece(&mut self, piece: Cow<'a, str>, span: Span) {
        let complete = piece.ends_with('\n');
        let block = self.block.as_mut().expect("inside a highlighted block");
        if block.carry.is_empty() && complete {
            self.line(piece, span);
        } else {
            block.carry.push_str(&piece);
            if complete {
                let line = std::mem::take(&mut block.carry);
                self.line(Cow::Owned(line), span);
            }
        }
    }

    fn text(&mut self, text: Cow<'a, str>, span: Span) {
        match text {
            Cow::Borrowed(s) => {
                for piece in s.split_inclusive('\n') {
                    self.piece(Cow::Borrowed(piece), span);
                }
            }
            Cow::Owned(s) => {
                for piece in s.split_inclusive('\n') {
                    self.piece(Cow::Owned(piece.to_string()), span);
                }
            }
        }
    }
}

impl<'a> Iterator for Blocks<'a> {
    type Item = Result<Spanned<Event<'a>>>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(item) = self.pending.pop_front() {
                return Some(item);
            }
            let Spanned { node, span } = match self.inner.next()? {
                Ok(spanned) => spanned,
                Err(e) => return Some(Err(e)),
            };
            match node {
                Event::Start(Tag::CodeBlock { lang, filename }) => {
                    let syntax = lang.as_deref().and_then(engine::syntax_for_token);
                    match syntax {
                        Some(syntax) => {
                            self.block = Some(Block {
                                lexer: Lexer::new(syntax),
                                carry: String::new(),
                            });
                            return Some(Ok(Spanned::new(Event::Start(Tag::Preformatted), span)));
                        }
                        None => {
                            return Some(Ok(Spanned::new(
                                Event::Start(Tag::CodeBlock { lang, filename }),
                                span,
                            )));
                        }
                    }
                }
                Event::Text(text) if self.block.is_some() => self.text(text, span),
                Event::End(TagKind::CodeBlock) if self.block.is_some() => {
                    // A last line with no newline is still a line.
                    let carry = std::mem::take(&mut self.block.as_mut().expect("block").carry);
                    if !carry.is_empty() {
                        self.line(Cow::Owned(carry), span);
                    }
                    self.block = None;
                    self.push(Event::End(TagKind::Preformatted), span);
                }
                other => return Some(Ok(Spanned::new(other, span))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termdoc_core::TokenRole;

    fn stream(events: Vec<Event<'static>>) -> Events<'static> {
        Box::new(events.into_iter().map(|e| Ok(Spanned::bare(e))))
    }

    fn run(events: Vec<Event<'static>>) -> Vec<Event<'static>> {
        CodeBlockHighlighter::new()
            .apply(stream(events))
            .map(|e| e.unwrap().node)
            .collect()
    }

    fn fenced(lang: Option<&'static str>, chunks: &[&'static str]) -> Vec<Event<'static>> {
        let mut events = vec![
            Event::Start(Tag::Paragraph),
            Event::Text("before".into()),
            Event::End(TagKind::Paragraph),
            Event::Start(Tag::CodeBlock {
                lang: lang.map(Cow::Borrowed),
                filename: None,
            }),
        ];
        events.extend(chunks.iter().map(|c| Event::Text(Cow::Borrowed(*c))));
        events.push(Event::End(TagKind::CodeBlock));
        events.push(Event::Start(Tag::Paragraph));
        events.push(Event::Text("after".into()));
        events.push(Event::End(TagKind::Paragraph));
        events
    }

    fn code_text(events: &[Event<'_>]) -> String {
        let mut inside = false;
        let mut out = String::new();
        for e in events {
            match e {
                Event::Start(Tag::Preformatted | Tag::CodeBlock { .. }) => inside = true,
                Event::End(TagKind::Preformatted | TagKind::CodeBlock) => inside = false,
                Event::Text(t) if inside => out.push_str(t),
                _ => {}
            }
        }
        out
    }

    fn roles(events: &[Event<'_>]) -> Vec<TokenRole> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Start(Tag::Token { role }) => Some(*role),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_block_with_a_known_language_becomes_highlighted_preformatted() {
        let out = run(fenced(
            Some("rust"),
            &["fn main() {}\n", "let x = 1; // c\n"],
        ));
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Start(Tag::Preformatted)))
        );
        assert!(
            !out.iter()
                .any(|e| matches!(e, Event::Start(Tag::CodeBlock { .. })))
        );
        let r = roles(&out);
        assert!(r.contains(&TokenRole::Keyword), "{r:?}");
        assert!(r.contains(&TokenRole::Comment), "{r:?}");
        assert_eq!(code_text(&out), "fn main() {}\nlet x = 1; // c\n");
    }

    #[test]
    fn the_rest_of_the_document_is_left_alone() {
        let out = run(fenced(Some("rust"), &["fn a() {}\n"]));
        assert_eq!(out.first(), Some(&Event::Start(Tag::Paragraph)));
        assert_eq!(out.last(), Some(&Event::End(TagKind::Paragraph)));
        assert!(out.contains(&Event::Text("before".into())));
        assert!(out.contains(&Event::Text("after".into())));
    }

    #[test]
    fn unknown_missing_and_plain_languages_pass_through_untouched() {
        for lang in [None, Some("nonsense-lang-xyz"), Some("text")] {
            let input = fenced(lang, &["fn main() {}\n"]);
            assert_eq!(run(input.clone()), input, "{lang:?}");
        }
    }

    #[test]
    fn a_line_split_across_chunks_is_joined_before_it_is_lexed() {
        // The parser may hand over text in any pieces. A keyword cut in two must still be one.
        let split = run(fenced(Some("rust"), &["f", "n ma", "in() {}\n"]));
        let whole = run(fenced(Some("rust"), &["fn main() {}\n"]));
        assert_eq!(code_text(&split), "fn main() {}\n");
        assert_eq!(roles(&split), roles(&whole));
    }

    #[test]
    fn a_last_line_with_no_newline_is_still_a_line() {
        let out = run(fenced(Some("rust"), &["fn a() {}\n", "let y = 2;"]));
        assert_eq!(code_text(&out), "fn a() {}\nlet y = 2;");
        assert!(roles(&out).contains(&TokenRole::Number));
    }

    #[test]
    fn several_lines_in_one_chunk_keep_their_state() {
        let out = run(fenced(Some("rust"), &["/* open\nfn not_code() {}\n*/\n"]));
        // The middle line is comment, not code.
        assert!(
            !roles(&out).contains(&TokenRole::Function),
            "{:?}",
            roles(&out)
        );
        assert_eq!(code_text(&out), "/* open\nfn not_code() {}\n*/\n");
    }

    #[test]
    fn the_stream_stays_balanced() {
        for lang in [Some("rust"), Some("python"), None] {
            let out = run(fenced(lang, &["x = 1\n", "def f(): pass"]));
            let opens = out.iter().filter(|e| matches!(e, Event::Start(_))).count();
            let closes = out.iter().filter(|e| matches!(e, Event::End(_))).count();
            assert_eq!(opens, closes, "{lang:?}");
        }
    }

    #[test]
    fn borrowed_text_stays_borrowed() {
        let out = run(fenced(Some("rust"), &["fn a() {}\n"]));
        for e in &out {
            if let Event::Text(t) = e {
                assert!(matches!(t, Cow::Borrowed(_)), "'{t}' was copied");
            }
        }
    }

    #[test]
    fn an_empty_block_is_still_a_block() {
        let out = run(fenced(Some("rust"), &[]));
        assert_eq!(code_text(&out), "");
        let opens = out.iter().filter(|e| matches!(e, Event::Start(_))).count();
        let closes = out.iter().filter(|e| matches!(e, Event::End(_))).count();
        assert_eq!(opens, closes);
    }
}
