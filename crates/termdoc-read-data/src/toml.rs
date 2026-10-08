//! The TOML reader.
//!
//! # Why this does not use the `toml` crate
//!
//! The plan (docs/HANDOFF.md §6.1) was to materialise the document with `toml` and pretty-print
//! it, "without apology, because config files are small". Config files are small, and they are
//! also made of comments: `Cargo.toml`, `pyproject.toml` and every hand-kept settings file
//! explain themselves in `#` lines, and a parse tree has nowhere to put them. Printing the
//! parsed document would show a different file than the one on disk, sorted and stripped.
//!
//! So this reader makes the same choice as the YAML one (see `yaml.rs`): it **highlights**
//! rather than parses. The text goes out byte for byte, one line at a time, as borrowed slices,
//! and only the colouring is decided here. It streams for free, and it needs no dependency.
//! The price is the same too: it is a lexer, not a validator, so invalid TOML is coloured like
//! valid TOML.
//!
//! # What it recognises
//!
//! `[table]` and `[[array.of.tables]]` headers, `key = value` with bare, dotted and quoted keys,
//! basic and literal strings, multi-line strings (`"""` and `'''`) across lines, integers in
//! every base, floats, booleans, dates and times, comments, and arrays and inline tables —
//! including ones that span lines, tracked by a bracket depth carried from line to line.
//!
//! Known limits, all of them mis-colouring and none of them losing text: a date and a time
//! separated by a space are coloured as two numbers; a key that is not followed by `=` on its
//! own line is coloured as a value.

use termdoc_core::{
    DocumentReader, Events, FormatId, ReadContext, ReaderCaps, Result, Source, TokenRole,
};

use crate::highlight::{HighlightEvents, Painter, Piece, skip_spaces, trim_end};

#[derive(Debug, Clone, Copy, Default)]
pub struct TomlReader;

impl TomlReader {
    pub fn new() -> Self {
        TomlReader
    }
}

impl DocumentReader for TomlReader {
    fn id(&self) -> FormatId {
        FormatId::Toml
    }

    fn capabilities(&self) -> ReaderCaps {
        ReaderCaps {
            // True: each line is classified on its own and borrowed from the source.
            streaming: true,
            paginated: false,
            metadata: false,
        }
    }

    fn read<'a>(&self, src: &'a Source, _ctx: &ReadContext) -> Result<Events<'a>> {
        Ok(Box::new(HighlightEvents::new(src, FormatId::Toml, lex)))
    }
}

/// What carries over from one line to the next. Everything else is decided per line.
#[derive(Debug, Default)]
struct State {
    /// Inside a multi-line string that has not closed yet: its quote character.
    open_multi: Option<u8>,
    /// How many arrays and inline tables are open, so a line in the middle of one is read as
    /// its contents and not as a new `key = value`.
    depth: usize,
}

/// Colours one line (without its terminator). The result covers `0..line.len()` exactly.
fn lex(line: &str, st: &mut State) -> Vec<Piece> {
    let b = line.as_bytes();
    let len = b.len();
    let mut p = Painter::new();

    // The rest of a multi-line string that began on an earlier line.
    if let Some(q) = st.open_multi {
        let start = skip_spaces(b, 0);
        match closing_triple(b, start, q) {
            Some(end) => {
                p.tok(start, end, TokenRole::String);
                st.open_multi = None;
                scan(b, end, st, &mut p);
            }
            None => p.tok(start, len, TokenRole::String),
        }
        return p.finish(len);
    }

    let indent = skip_spaces(b, 0);
    if indent == len {
        return p.finish(len);
    }

    // Inside a multi-line array or inline table, the line is contents, not a statement.
    if st.depth > 0 {
        scan(b, 0, st, &mut p);
        return p.finish(len);
    }

    match b[indent] {
        b'#' => p.tok(indent, len, TokenRole::Comment),
        b'[' => header(b, indent, &mut p),
        _ => match find_equals(b, indent) {
            Some(eq) => {
                p.tok(indent, trim_end(b, indent, eq), TokenRole::Key);
                p.tok(eq, eq + 1, TokenRole::Punctuation);
                scan(b, eq + 1, st, &mut p);
            }
            None => scan(b, indent, st, &mut p),
        },
    }
    p.finish(len)
}

/// `[name]` or `[[name]]`, then an optional comment.
fn header(b: &[u8], from: usize, p: &mut Painter) {
    let len = b.len();
    let n = if b.get(from + 1) == Some(&b'[') { 2 } else { 1 };
    p.tok(from, from + n, TokenRole::Punctuation);

    // The name may be quoted and may contain `]` inside the quotes.
    let name_start = from + n;
    let mut j = name_start;
    while j < len {
        match b[j] {
            q @ (b'"' | b'\'') => match closing_quote(b, j + 1, q) {
                Some(end) => j = end,
                None => j = len,
            },
            b']' => break,
            _ => j += 1,
        }
    }
    if j >= len {
        // Never closed: still a name, and the text is not lost.
        p.tok(name_start, trim_end(b, name_start, len), TokenRole::Name);
        return;
    }
    let name_start = skip_spaces(b, name_start);
    p.tok(name_start, trim_end(b, name_start, j), TokenRole::Name);
    p.tok(j, (j + n).min(len), TokenRole::Punctuation);
    let after = skip_spaces(b, j + n);
    if after < len && b[after] == b'#' {
        p.tok(after, len, TokenRole::Comment);
    }
}

/// Colours values from `from` to the end of the line: strings, scalars, the punctuation of
/// arrays and inline tables, and a trailing comment. Also used for the contents of a
/// multi-line array, which is why a key inside an inline table is recognised by looking ahead
/// for `=` rather than by position.
fn scan(b: &[u8], from: usize, st: &mut State, p: &mut Painter) {
    let len = b.len();
    let mut i = from;
    while i < len {
        match b[i] {
            b' ' | b'\t' => i += 1,
            b'#' => {
                p.tok(i, len, TokenRole::Comment);
                return;
            }
            b'[' | b'{' => {
                st.depth += 1;
                p.tok(i, i + 1, TokenRole::Punctuation);
                i += 1;
            }
            b']' | b'}' => {
                st.depth = st.depth.saturating_sub(1);
                p.tok(i, i + 1, TokenRole::Punctuation);
                i += 1;
            }
            b',' | b'=' => {
                p.tok(i, i + 1, TokenRole::Punctuation);
                i += 1;
            }
            q @ (b'"' | b'\'') => {
                if b[i..].starts_with(&[q, q, q]) {
                    match closing_triple(b, i + 3, q) {
                        Some(end) => {
                            p.tok(i, end, TokenRole::String);
                            i = end;
                        }
                        None => {
                            p.tok(i, len, TokenRole::String);
                            st.open_multi = Some(q);
                            return;
                        }
                    }
                } else {
                    let end = closing_quote(b, i + 1, q).unwrap_or(len);
                    let role = if equals_follows(b, end) {
                        TokenRole::Key
                    } else {
                        TokenRole::String
                    };
                    p.tok(i, end, role);
                    i = end;
                }
            }
            _ => {
                let mut j = i;
                while j < len
                    && !matches!(
                        b[j],
                        b' ' | b'\t'
                            | b','
                            | b']'
                            | b'}'
                            | b'['
                            | b'{'
                            | b'='
                            | b'#'
                            | b'"'
                            | b'\''
                    )
                {
                    j += 1;
                }
                let role = if equals_follows(b, j) {
                    TokenRole::Key
                } else {
                    classify(&b[i..j])
                };
                p.tok(i, j, role);
                i = j;
            }
        }
    }
}

/// The `=` of a `key = value` line, skipping any quoted part of the key.
fn find_equals(b: &[u8], from: usize) -> Option<usize> {
    let mut j = from;
    while j < b.len() {
        match b[j] {
            b'=' => return Some(j),
            b'#' => return None,
            q @ (b'"' | b'\'') => j = closing_quote(b, j + 1, q)?,
            _ => j += 1,
        }
    }
    None
}

/// Whether the next thing after `from`, past spaces, is `=`.
fn equals_follows(b: &[u8], from: usize) -> bool {
    let k = skip_spaces(b, from);
    k < b.len() && b[k] == b'='
}

/// The index just past the quote that closes a single-line string opened with `q`. Basic
/// strings (`"`) have backslash escapes; literal strings (`'`) have none.
fn closing_quote(b: &[u8], from: usize, q: u8) -> Option<usize> {
    let mut k = from;
    while k < b.len() {
        if q == b'"' && b[k] == b'\\' {
            k += 2;
            continue;
        }
        if b[k] == q {
            return Some(k + 1);
        }
        k += 1;
    }
    None
}

/// The index just past the `"""` or `'''` that closes a multi-line string.
fn closing_triple(b: &[u8], from: usize, q: u8) -> Option<usize> {
    let mut k = from;
    while k < b.len() {
        if q == b'"' && b[k] == b'\\' {
            k += 2;
            continue;
        }
        if b[k] == q && b[k..].starts_with(&[q, q, q]) {
            return Some(k + 3);
        }
        k += 1;
    }
    None
}

fn classify(text: &[u8]) -> TokenRole {
    let Ok(s) = std::str::from_utf8(text) else {
        return TokenRole::String;
    };
    match s {
        "true" | "false" => TokenRole::Bool,
        _ if is_number(s) => TokenRole::Number,
        _ => TokenRole::String,
    }
}

/// Integers (any base), floats, `inf`/`nan`, and dates and times, which TOML writes with the
/// same few characters and which read as numbers in a terminal.
fn is_number(s: &str) -> bool {
    let u = s.strip_prefix(['+', '-']).unwrap_or(s);
    if matches!(u, "inf" | "nan") {
        return true;
    }
    for (prefix, valid) in [
        ("0x", (|c: u8| c.is_ascii_hexdigit()) as fn(u8) -> bool),
        ("0o", |c: u8| (b'0'..=b'7').contains(&c)),
        ("0b", |c: u8| c == b'0' || c == b'1'),
    ] {
        if let Some(digits) = u.strip_prefix(prefix) {
            return !digits.is_empty() && digits.bytes().all(|c| c == b'_' || valid(c));
        }
    }
    u.bytes().next().is_some_and(|c| c.is_ascii_digit())
        && u.bytes().all(|c| {
            c.is_ascii_digit()
                || matches!(c, b'_' | b'.' | b'e' | b'E' | b'+' | b'-' | b':')
                || matches!(c, b'T' | b't' | b'Z' | b'z')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;
    use termdoc_core::{Event, Tag, TagKind};

    /// A line as `{role|text}` runs, so a test reads like the colouring it checks.
    fn show(line: &str, st: &mut State) -> String {
        lex(line, st)
            .into_iter()
            .map(|(r, role)| {
                let t = &line[r];
                match role {
                    Some(role) => format!("{{{role:?}|{t}}}"),
                    None => t.to_string(),
                }
            })
            .collect()
    }

    fn one(line: &str) -> String {
        show(line, &mut State::default())
    }

    #[test]
    fn a_key_and_its_value_get_different_roles() {
        assert_eq!(
            one(r#"name = "termdoc""#),
            r#"{Key|name} {Punctuation|=} {String|"termdoc"}"#
        );
    }

    #[test]
    fn values_are_classified() {
        assert_eq!(one("a = 42"), "{Key|a} {Punctuation|=} {Number|42}");
        assert_eq!(one("a = 1_000"), "{Key|a} {Punctuation|=} {Number|1_000}");
        assert_eq!(one("a = -1.5e3"), "{Key|a} {Punctuation|=} {Number|-1.5e3}");
        assert_eq!(one("a = 0xFF"), "{Key|a} {Punctuation|=} {Number|0xFF}");
        assert_eq!(one("a = inf"), "{Key|a} {Punctuation|=} {Number|inf}");
        assert_eq!(one("a = true"), "{Key|a} {Punctuation|=} {Bool|true}");
        assert_eq!(
            one("d = 1979-05-27T07:32:00Z"),
            "{Key|d} {Punctuation|=} {Number|1979-05-27T07:32:00Z}"
        );
        assert_eq!(one("a = 'lit'"), "{Key|a} {Punctuation|=} {String|'lit'}");
    }

    #[test]
    fn tables_and_arrays_of_tables() {
        assert_eq!(
            one("[package]"),
            "{Punctuation|[}{Name|package}{Punctuation|]}"
        );
        assert_eq!(
            one("[[bin]] # one per binary"),
            "{Punctuation|[[}{Name|bin}{Punctuation|]]} {Comment|# one per binary}"
        );
        assert_eq!(
            one(r#"[a."b]c".d]"#),
            r#"{Punctuation|[}{Name|a."b]c".d}{Punctuation|]}"#
        );
    }

    #[test]
    fn dotted_and_quoted_keys() {
        assert_eq!(one("a.b.c = 1"), "{Key|a.b.c} {Punctuation|=} {Number|1}");
        assert_eq!(
            one(r#""a = b" = 1"#),
            r#"{Key|"a = b"} {Punctuation|=} {Number|1}"#
        );
    }

    #[test]
    fn a_comment_is_kept_and_a_hash_in_a_string_is_not_one() {
        assert_eq!(one("# note"), "{Comment|# note}");
        assert_eq!(
            one("a = 1 # why"),
            "{Key|a} {Punctuation|=} {Number|1} {Comment|# why}"
        );
        assert_eq!(
            one(r##"a = "x # y""##),
            r##"{Key|a} {Punctuation|=} {String|"x # y"}"##
        );
    }

    #[test]
    fn inline_tables_and_arrays_on_one_line() {
        assert_eq!(
            one(r#"p = { x = 1, y = "s" }"#),
            r#"{Key|p} {Punctuation|=} {Punctuation|{} {Key|x} {Punctuation|=} {Number|1}{Punctuation|,} {Key|y} {Punctuation|=} {String|"s"} {Punctuation|}}"#
        );
        assert_eq!(
            one("a = [1, true]"),
            "{Key|a} {Punctuation|=} {Punctuation|[}{Number|1}{Punctuation|,} {Bool|true}{Punctuation|]}"
        );
    }

    #[test]
    fn an_array_can_span_lines() {
        let mut st = State::default();
        assert_eq!(
            show("deps = [", &mut st),
            "{Key|deps} {Punctuation|=} {Punctuation|[}"
        );
        assert_eq!(st.depth, 1);
        // What would be a `key = value` statement anywhere else is contents here.
        assert_eq!(
            show(r#"  "a", # first"#, &mut st),
            r#"  {String|"a"}{Punctuation|,} {Comment|# first}"#
        );
        assert_eq!(show("]", &mut st), "{Punctuation|]}");
        assert_eq!(st.depth, 0);
        assert_eq!(
            show("next = 1", &mut st),
            "{Key|next} {Punctuation|=} {Number|1}"
        );
    }

    #[test]
    fn a_multi_line_string_is_a_string_until_it_closes() {
        let mut st = State::default();
        assert_eq!(
            show(r#"s = """first"#, &mut st),
            r#"{Key|s} {Punctuation|=} {String|"""first}"#
        );
        // `key = value` shapes inside the string are text.
        assert_eq!(
            show("  a = 1 # not a comment", &mut st),
            "  {String|a = 1 # not a comment}"
        );
        assert_eq!(
            show(r#"end""" # done"#, &mut st),
            r#"{String|end"""} {Comment|# done}"#
        );
        assert!(st.open_multi.is_none());
        assert_eq!(show("b = 2", &mut st), "{Key|b} {Punctuation|=} {Number|2}");
    }

    #[test]
    fn a_literal_multi_line_string_has_no_escapes() {
        let mut st = State::default();
        show("s = '''", &mut st);
        assert_eq!(st.open_multi, Some(b'\''));
        assert_eq!(show(r"C:\path\", &mut st), r"{String|C:\path\}");
        show("'''", &mut st);
        assert!(st.open_multi.is_none());
    }

    #[test]
    fn the_pieces_always_cover_the_whole_line() {
        // A lost character is the one failure a viewer cannot have.
        for line in [
            "",
            "   ",
            "[a",
            "[[",
            "a = ",
            "= 1",
            "a = \"unterminated",
            "a = [1, { b = 2 }] # c",
            "\tkey\t=\tvalue",
            "é = \"ü\"",
            "a = '''",
            "]]}} =",
        ] {
            let joined: String = lex(line, &mut State::default())
                .into_iter()
                .map(|(r, _)| &line[r])
                .collect();
            assert_eq!(joined, line);
        }
    }

    // ---- the reader

    fn events(bytes: &[u8]) -> Vec<Event<'static>> {
        // Leaked on purpose: the events borrow the source, and a test does not care.
        let src: &'static Source =
            Box::leak(Box::new(Source::from_bytes("t.toml", bytes.to_vec())));
        TomlReader::new()
            .read(src, &ReadContext::default())
            .unwrap()
            .map(|e| e.unwrap().node)
            .collect()
    }

    #[test]
    fn the_text_survives_byte_for_byte() {
        let input =
            "# c\n[package]\nname = \"x\" # y\nl = [\n  1,\n]\ns = \"\"\"\nkeep = this\n\"\"\"\n";
        let out: String = events(input.as_bytes())
            .into_iter()
            .filter_map(|e| match e {
                Event::Text(t) => Some(t.into_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(out, input);
    }

    #[test]
    fn the_stream_is_balanced_and_nothing_is_copied() {
        let ev = events(b"[a]\nb = 1 # c\n");
        assert!(matches!(ev.first(), Some(Event::Start(Tag::Document(_)))));
        assert!(matches!(ev.last(), Some(Event::End(TagKind::Document))));
        let opens = ev.iter().filter(|e| matches!(e, Event::Start(_))).count();
        let closes = ev.iter().filter(|e| matches!(e, Event::End(_))).count();
        assert_eq!(opens, closes);
        for e in ev {
            if let Event::Text(t) = e {
                assert!(matches!(t, Cow::Borrowed(_)), "'{t}' was copied");
            }
        }
    }

    #[test]
    fn capabilities_claim_streaming_because_it_streams() {
        assert!(TomlReader::new().capabilities().streaming);
    }
}
