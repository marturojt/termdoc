//! The log reader: severity and time, found line by line.
//!
//! A log is read down its levels. This colours the three things that make that possible and
//! nothing else, so the rest of a line is the line:
//!
//! - the **timestamp** at the start, receding;
//! - the **level** — `ERROR`, `warn`, `[INFO]`, `level=debug` — by how worrying it is;
//! - the **keys** of `key=value` pairs, so structured (logfmt) lines can be scanned.
//!
//! It highlights rather than parses, for the reason YAML does (`yaml.rs` in `termdoc-read-data`):
//! the text goes out byte for byte, a line is classified on its own, and nothing is held. So it
//! streams, from a file or from a pipe that has not ended.
//!
//! The timestamp is found by `termdoc_core::timestamp`, the parser detection uses to decide
//! that a file is a log in the first place, so the two cannot disagree about what a timestamp
//! is.
//!
//! # What counts as a level
//!
//! One of the first few words after the timestamp, so that a line saying "the fatal error was
//! ignored" is not coloured as a fatal one. A line with no timestamp gets only its first two
//! words considered, and an indented one (a stack trace) none. `level=`, `lvl=`, `severity=`
//! and `loglevel=` take their value as the level wherever they are.

use termdoc_core::highlight::{HighlightEvents, Painter, Piece};
use termdoc_core::stream::LineStream;
use termdoc_core::timestamp::leading_timestamp;
use termdoc_core::{
    DocumentReader, Events, FormatId, ReadContext, ReaderCaps, Result, Source, TokenRole,
};

use crate::text::{TextReader, lex_plain};

/// How many words after the timestamp may be a level.
const WORDS_WITH_TIMESTAMP: usize = 4;
/// … and on a line that has none.
const WORDS_WITHOUT: usize = 2;

#[derive(Debug, Clone, Copy)]
pub struct LogReader {
    /// What reads a log when there is no colour to show: the plain reader, which is the fastest
    /// way through a file and gives exactly the same text.
    plain: TextReader,
}

impl Default for LogReader {
    fn default() -> Self {
        Self::new()
    }
}

impl LogReader {
    pub fn new() -> Self {
        LogReader {
            plain: TextReader::log(),
        }
    }
}

impl DocumentReader for LogReader {
    fn id(&self) -> FormatId {
        FormatId::Log
    }

    fn capabilities(&self) -> ReaderCaps {
        ReaderCaps {
            streaming: true,
            paginated: false,
            metadata: false,
        }
    }

    fn read<'a>(&self, src: &'a Source, ctx: &ReadContext) -> Result<Events<'a>> {
        if !ctx.styled {
            // Nothing could show the colours, and a log can be hundreds of megabytes.
            return self.plain.read(src, ctx);
        }
        Ok(Box::new(HighlightEvents::new(src, FormatId::Log, (), lex)))
    }

    fn streams_input(&self) -> bool {
        true
    }

    fn read_stream<'a>(&self, stream: LineStream, ctx: &ReadContext) -> Result<Events<'a>> {
        let lex = if ctx.styled { lex } else { lex_plain };
        Ok(Box::new(HighlightEvents::from_stream(
            stream,
            FormatId::Log,
            (),
            lex,
        )))
    }
}

fn level_of(word: &[u8]) -> Option<TokenRole> {
    let is = |names: &[&str]| {
        names
            .iter()
            .any(|n| word.eq_ignore_ascii_case(n.as_bytes()))
    };
    if is(&["trace", "verbose", "finest", "finer"]) {
        Some(TokenRole::LevelTrace)
    } else if is(&["debug", "dbg", "fine"]) {
        Some(TokenRole::LevelDebug)
    } else if is(&["info", "information", "notice"]) {
        Some(TokenRole::LevelInfo)
    } else if is(&["warn", "warning", "wrn"]) {
        Some(TokenRole::LevelWarn)
    } else if is(&[
        "err",
        "error",
        "fatal",
        "crit",
        "critical",
        "panic",
        "severe",
        "emerg",
        "emergency",
        "alert",
    ]) {
        Some(TokenRole::LevelError)
    } else {
        None
    }
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// A key of a `key=value` pair: it starts a token and runs to the `=`.
fn is_key_char(c: u8) -> bool {
    is_word(c) || matches!(c, b'.' | b'-')
}

/// The words of `b[from..]`, as ranges.
fn words(b: &[u8], from: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut i = from;
    std::iter::from_fn(move || {
        while i < b.len() && !is_word(b[i]) {
            i += 1;
        }
        if i >= b.len() {
            return None;
        }
        let start = i;
        while i < b.len() && is_word(b[i]) {
            i += 1;
        }
        Some((start, i))
    })
}

type Mark = (usize, usize, TokenRole);

/// The first word after the timestamp that names a level, within `limit` words.
fn find_level(b: &[u8], from: usize, limit: usize) -> Option<Mark> {
    for (start, end) in words(b, from).take(limit) {
        // `level=error`: the key is not the level; its value is, and `find_pairs` finds that.
        if b.get(end) == Some(&b'=') {
            continue;
        }
        if let Some(role) = level_of(&b[start..end]) {
            return Some((start, end, role));
        }
    }
    None
}

/// `key=value` pairs: pushes each key, and returns the value of the first `level=` as a level.
///
/// A key must start a token, so `?a=1&b=2` in a URL is left alone.
fn find_pairs(b: &[u8], from: usize, marks: &mut Vec<Mark>) -> Option<Mark> {
    let mut level = None;
    let mut i = from;
    while i < b.len() {
        let starts_token = i == 0 || matches!(b[i - 1], b' ' | b'\t' | b',' | b'{' | b'(' | b'[');
        if !(starts_token && (b[i].is_ascii_alphabetic() || b[i] == b'_')) {
            i += 1;
            continue;
        }
        let mut end = i;
        while end < b.len() && is_key_char(b[end]) {
            end += 1;
        }
        let is_pair =
            b.get(end) == Some(&b'=') && b.get(end + 1).is_some_and(|c| !c.is_ascii_whitespace());
        if !is_pair {
            i = end.max(i + 1);
            continue;
        }
        marks.push((i, end, TokenRole::Key));
        let key = &b[i..end];
        if level.is_none()
            && ["level", "lvl", "severity", "loglevel"]
                .iter()
                .any(|k| key.eq_ignore_ascii_case(k.as_bytes()))
        {
            // The value may be quoted: `level="warn"`.
            let mut v = end + 1;
            if matches!(b.get(v), Some(b'"' | b'\'')) {
                v += 1;
            }
            let mut ve = v;
            while ve < b.len() && is_word(b[ve]) {
                ve += 1;
            }
            if let Some(role) = level_of(&b[v..ve]) {
                level = Some((v, ve, role));
            }
        }
        i = end + 1;
    }
    level
}

/// Colours one line (without its terminator). The result covers `0..line.len()` exactly.
pub(crate) fn lex(line: &str, _state: &mut ()) -> Vec<Piece> {
    let b = line.as_bytes();
    let mut marks: Vec<Mark> = Vec::new();

    let stamp = leading_timestamp(line);
    let after = stamp.as_ref().map_or(0, |r| r.end);
    if let Some(r) = &stamp {
        marks.push((r.start, r.end, TokenRole::Timestamp));
    }

    // An indented line with no timestamp is a continuation — a stack frame, a wrapped message —
    // and its first words are not a level.
    let continuation = stamp.is_none() && b.first().is_some_and(|c| *c == b' ' || *c == b'\t');
    let word_level = if continuation {
        None
    } else {
        let limit = if stamp.is_some() {
            WORDS_WITH_TIMESTAMP
        } else {
            WORDS_WITHOUT
        };
        find_level(b, after, limit)
    };

    // Only worth the scan if there is a pair to find.
    let pair_level = if b.contains(&b'=') {
        find_pairs(b, after, &mut marks)
    } else {
        None
    };

    // An explicit `level=` is the better evidence, and a line has one level.
    if let Some(level) = pair_level.or(word_level) {
        marks.push(level);
    }

    marks.sort_by_key(|(start, _, _)| *start);
    let mut p = Painter::new();
    for (start, end, role) in marks {
        p.tok(start, end, role);
    }
    p.finish(b.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line as `{role|text}` runs, so a test reads like the colouring it checks.
    fn show(line: &str) -> String {
        lex(line, &mut ())
            .into_iter()
            .map(|(r, role)| match role {
                Some(role) => format!("{{{role:?}|{}}}", &line[r]),
                None => line[r].to_string(),
            })
            .collect()
    }

    #[test]
    fn a_timestamp_and_a_level_are_found() {
        assert_eq!(
            show("2026-08-10T12:00:00Z INFO server started"),
            "{Timestamp|2026-08-10T12:00:00Z} {LevelInfo|INFO} server started"
        );
        assert_eq!(
            show("2026-08-10 12:00:00,123 [main] ERROR boom"),
            "{Timestamp|2026-08-10 12:00:00,123} [main] {LevelError|ERROR} boom"
        );
    }

    #[test]
    fn every_level_name_maps_to_a_level() {
        for (word, role) in [
            ("TRACE", TokenRole::LevelTrace),
            ("debug", TokenRole::LevelDebug),
            ("Info", TokenRole::LevelInfo),
            ("notice", TokenRole::LevelInfo),
            ("WARN", TokenRole::LevelWarn),
            ("warning", TokenRole::LevelWarn),
            ("ERROR", TokenRole::LevelError),
            ("fatal", TokenRole::LevelError),
            ("CRITICAL", TokenRole::LevelError),
            ("panic", TokenRole::LevelError),
        ] {
            let line = format!("12:00:00 {word} message");
            assert_eq!(
                show(&line),
                format!("{{Timestamp|12:00:00}} {{{role:?}|{word}}} message")
            );
        }
    }

    #[test]
    fn brackets_and_colons_around_a_level_stay_plain() {
        assert_eq!(
            show("12:00:00 [WARN] careful"),
            "{Timestamp|12:00:00} [{LevelWarn|WARN}] careful"
        );
        assert_eq!(
            show("Aug 10 12:00:00 host app: error: bad"),
            "{Timestamp|Aug 10 12:00:00} host app: {LevelError|error}: bad"
        );
    }

    #[test]
    fn a_word_late_in_the_message_is_not_a_level() {
        // The error is *in* the message, which is INFO.
        assert_eq!(
            show("2026-08-10 12:00:00 INFO the fatal error was ignored"),
            "{Timestamp|2026-08-10 12:00:00} {LevelInfo|INFO} the fatal error was ignored"
        );
        assert_eq!(
            show("2026-08-10 12:00:00 server could not read the error log"),
            "{Timestamp|2026-08-10 12:00:00} server could not read the error log"
        );
    }

    #[test]
    fn a_line_without_a_timestamp_still_has_its_leading_level() {
        assert_eq!(show("ERROR cannot open"), "{LevelError|ERROR} cannot open");
        assert_eq!(show("it is an ERROR"), "it is an ERROR");
    }

    #[test]
    fn a_continuation_line_is_left_alone() {
        assert_eq!(
            show("    at Foo.bar(Foo.java:12)"),
            "    at Foo.bar(Foo.java:12)"
        );
        assert_eq!(
            show("  ERROR in an indented line"),
            "  ERROR in an indented line"
        );
        assert_eq!(
            show("Caused by: java.io.IOException"),
            "Caused by: java.io.IOException"
        );
    }

    #[test]
    fn logfmt_keys_are_marked_and_level_pairs_set_the_level() {
        assert_eq!(
            show("ts=12:00:00 level=error msg=\"disk full\" host=a1"),
            "{Key|ts}=12:00:00 {Key|level}={LevelError|error} {Key|msg}=\"disk full\" {Key|host}=a1"
        );
        assert_eq!(
            show("level=\"warn\" retry=3"),
            "{Key|level}=\"{LevelWarn|warn}\" {Key|retry}=3"
        );
    }

    #[test]
    fn a_level_pair_wins_over_a_level_word() {
        // One level per line, and the explicit one is the one that means it.
        assert_eq!(
            show("2026-08-10 12:00:00 INFO retrying level=warn"),
            "{Timestamp|2026-08-10 12:00:00} INFO retrying {Key|level}={LevelWarn|warn}"
        );
    }

    #[test]
    fn urls_and_prose_are_not_pairs() {
        assert_eq!(
            show("12:00:00 GET /search?q=rust&page=2 200"),
            "{Timestamp|12:00:00} GET /search?q=rust&page=2 200"
        );
        assert_eq!(show("a == b and c = d"), "a == b and c = d");
    }

    #[test]
    fn the_pieces_always_cover_the_line() {
        // A lost character is the one failure a viewer cannot have.
        for line in [
            "",
            "   ",
            "=",
            "=x",
            "a=",
            "level=",
            "level=\"",
            "2026-08-10",
            "[",
            "[12:00:00",
            "é=ü ERROR 日本語=x",
            "12:00:00 ERROR=ERROR level=error level=warn",
            "key=value=value another=",
        ] {
            let joined: String = lex(line, &mut ())
                .into_iter()
                .map(|(r, _)| &line[r])
                .collect();
            assert_eq!(joined, line, "{line:?}");
        }
    }
}

#[cfg(test)]
mod reader_tests {
    use super::*;
    use std::io::Cursor;
    use termdoc_core::stream::StdinFeed;
    use termdoc_core::{Event, Tag, TagKind};

    const LOG: &str = "2026-08-10T12:00:00Z INFO server started port=8080\n\
                       2026-08-10T12:00:01Z WARN slow request\n\
                       \x20   at handler (main.rs:12)\n\
                       2026-08-10T12:00:02Z ERROR boom";

    fn from_source(input: &str, styled: bool) -> Vec<Event<'static>> {
        // Leaked on purpose: the events borrow the source, and a test does not care.
        let src: &'static Source = Box::leak(Box::new(Source::from_bytes(
            "t.log",
            input.as_bytes().to_vec(),
        )));
        let ctx = ReadContext {
            styled,
            ..ReadContext::default()
        };
        LogReader::new()
            .read(src, &ctx)
            .unwrap()
            .map(|e| e.unwrap().node)
            .collect()
    }

    fn from_stream(input: &str, styled: bool) -> Vec<Event<'static>> {
        let stream = StdinFeed::from_reader(Cursor::new(input.as_bytes().to_vec()))
            .into_lines(encoding_rs_utf8());
        let ctx = ReadContext {
            styled,
            ..ReadContext::default()
        };
        LogReader::new()
            .read_stream(stream, &ctx)
            .unwrap()
            .map(|e| e.unwrap().node)
            .collect()
    }

    fn encoding_rs_utf8() -> &'static termdoc_core::stream::Encoding {
        termdoc_core::stream::UTF_8
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
    fn the_text_survives_byte_for_byte_in_every_mode() {
        for styled in [true, false] {
            assert_eq!(
                text_of(&from_source(LOG, styled)),
                LOG,
                "source, styled={styled}"
            );
            assert_eq!(
                text_of(&from_stream(LOG, styled)),
                LOG,
                "stream, styled={styled}"
            );
        }
    }

    #[test]
    fn levels_timestamps_and_keys_are_coloured() {
        let r = roles(&from_source(LOG, true));
        for wanted in [
            TokenRole::Timestamp,
            TokenRole::LevelInfo,
            TokenRole::LevelWarn,
            TokenRole::LevelError,
            TokenRole::Key,
        ] {
            assert!(r.contains(&wanted), "no {wanted:?} in {r:?}");
        }
    }

    #[test]
    fn a_stream_is_coloured_exactly_like_the_file_it_came_from() {
        // The two paths share a lexer, and this is what keeps them from drifting.
        assert_eq!(
            roles(&from_stream(LOG, true)),
            roles(&from_source(LOG, true))
        );
    }

    #[test]
    fn without_colour_nothing_is_lexed() {
        assert!(roles(&from_source(LOG, false)).is_empty());
        assert!(roles(&from_stream(LOG, false)).is_empty());
    }

    #[test]
    fn streams_are_balanced_and_say_they_can_stream() {
        let ev = from_stream(LOG, true);
        let opens = ev.iter().filter(|e| matches!(e, Event::Start(_))).count();
        let closes = ev.iter().filter(|e| matches!(e, Event::End(_))).count();
        assert_eq!(opens, closes);
        assert!(matches!(ev.last(), Some(Event::End(TagKind::Document))));
        assert!(LogReader::new().streams_input());
        assert!(TextReader::plain().streams_input());
    }

    #[test]
    fn a_plain_line_is_one_event_and_not_two() {
        // Two events a line (the text, then its terminator) would double the work of a log.
        let ev = from_stream("nothing to colour here\nnor here\n", false);
        let texts = ev.iter().filter(|e| matches!(e, Event::Text(_))).count();
        assert_eq!(texts, 2);
    }
}
