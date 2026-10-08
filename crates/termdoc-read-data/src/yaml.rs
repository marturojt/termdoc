//! The YAML reader.
//!
//! # Why this does not use a YAML parser
//!
//! docs/DESIGN.md §9 recommended `yaml-rust2`, an event parser, and this reader does not use
//! it. The reason is what a viewer is for: a YAML parser reports *data*, and the data of a
//! config file is the least interesting thing in it. The comments are what explain the
//! settings, and the exact shape — the order, the quoting, the anchors — is what the person
//! came to look at. Every parser event stream drops the comments and normalises the rest, so a
//! reader built on one shows a different document than the one on disk. This one does not
//! parse YAML, it **highlights** it: the text goes out byte for byte, and only its colouring
//! is decided here.
//!
//! That also makes it genuinely streaming. A line is classified on its own, with two bits of
//! state carried between lines (see [`State`]), so `termdoc big.yaml | head` reads a few pages
//! and every line is a borrowed slice of the `mmap`.
//!
//! The price is that it is a lexer, not a validator. Invalid YAML is coloured like valid YAML
//! and no error is reported; `--explain` is the place where "is this really YAML" is answered.
//!
//! # What it recognises
//!
//! Keys, comments, `-` and `?` markers, `---` / `...`, `%` directives, anchors (`&a`), aliases
//! (`*a`) and tags (`!t`), quoted and plain scalars (classified as string, number, bool or
//! null by the YAML 1.2 core rules — so `yes` and `no` are strings), block scalars (`|`, `>`)
//! and their indented bodies, multi-line quoted scalars, and flow collections on one line.
//!
//! Known limits, all of them mis-colouring and none of them losing text: a flow collection
//! that spans lines is coloured line by line; a multi-line *plain* scalar is coloured as if each
//! line were its own value.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::ops::Range;

use termdoc_core::{
    Diagnostic, DocumentReader, Event, Events, FormatId, Metadata, ReadContext, ReaderCaps, Result,
    Source, Span, Spanned, Tag, TagKind, TokenRole,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct YamlReader;

impl YamlReader {
    pub fn new() -> Self {
        YamlReader
    }
}

impl DocumentReader for YamlReader {
    fn id(&self) -> FormatId {
        FormatId::Yaml
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
        Ok(Box::new(YamlEvents::new(src)))
    }
}

/// What carries over from one line to the next. Everything else is decided per line.
#[derive(Debug, Default)]
struct State {
    /// Inside a block scalar (`|` or `>`): the column of the node that owns it. Lines indented
    /// deeper than that are its body.
    block_parent: Option<usize>,
    /// Inside a quoted scalar that has not closed yet: the quote character.
    open_quote: Option<u8>,
}

/// A coloured run of one line, as a byte range of its body.
type Piece = (Range<usize>, Option<TokenRole>);

struct YamlEvents<'a> {
    src: &'a Source,
    bytes: &'a [u8],
    pos: usize,
    line: u32,
    state: State,
    pending: VecDeque<Spanned<Event<'a>>>,
    warned_encoding: bool,
    done: bool,
}

impl<'a> YamlEvents<'a> {
    fn new(src: &'a Source) -> Self {
        let mut pending = VecDeque::new();
        pending.push_back(Spanned::bare(Event::Start(Tag::Document(Box::new(
            Metadata {
                source_format: Some(FormatId::Yaml),
                ..Metadata::default()
            },
        )))));
        pending.push_back(Spanned::bare(Event::Start(Tag::Preformatted)));
        YamlEvents {
            src,
            bytes: src.bytes(),
            pos: 0,
            line: 0,
            state: State::default(),
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
        let pieces = lex(&line[..body_len], &mut self.state);
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

impl<'a> Iterator for YamlEvents<'a> {
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

// ---------------------------------------------------------------------------- lexer

/// Accumulates coloured runs and fills the gaps between them with uncoloured ones, so the
/// pieces always cover the whole line.
struct Painter {
    pieces: Vec<Piece>,
    at: usize,
}

impl Painter {
    fn new() -> Self {
        Painter {
            pieces: Vec::new(),
            at: 0,
        }
    }

    fn tok(&mut self, from: usize, to: usize, role: TokenRole) {
        if from >= to || from < self.at {
            return;
        }
        if from > self.at {
            self.pieces.push((self.at..from, None));
        }
        self.pieces.push((from..to, Some(role)));
        self.at = to;
    }

    fn finish(mut self, len: usize) -> Vec<Piece> {
        if self.at < len {
            self.pieces.push((self.at..len, None));
        }
        self.pieces
    }
}

/// Colours one line (without its terminator). The result covers `0..line.len()` exactly.
fn lex(line: &str, st: &mut State) -> Vec<Piece> {
    let b = line.as_bytes();
    let len = b.len();
    let mut p = Painter::new();

    // The rest of a quoted scalar that began on an earlier line.
    if let Some(q) = st.open_quote {
        let start = skip_spaces(b, 0);
        match closing_quote(b, start, q) {
            Some(end) => {
                p.tok(start, end, TokenRole::String);
                st.open_quote = None;
                trailer(b, end, &mut p);
            }
            None => p.tok(start, len, TokenRole::String),
        }
        return p.finish(len);
    }

    let indent = b.iter().take_while(|c| **c == b' ' || **c == b'\t').count();

    // The body of a block scalar, up to the first line that is no deeper than its owner.
    if let Some(parent) = st.block_parent {
        if indent == len {
            return p.finish(len);
        }
        if indent > parent {
            p.tok(indent, len, TokenRole::String);
            return p.finish(len);
        }
        st.block_parent = None;
    }

    if indent == len {
        return p.finish(len);
    }
    match b[indent] {
        b'#' => {
            p.tok(indent, len, TokenRole::Comment);
            return p.finish(len);
        }
        b'%' if indent == 0 => {
            p.tok(0, len, TokenRole::Attribute);
            return p.finish(len);
        }
        _ => {}
    }
    if indent == 0
        && (line.starts_with("---") || line.starts_with("..."))
        && (len == 3 || b[3] == b' ')
    {
        p.tok(0, 3, TokenRole::Punctuation);
        value(b, 3, 0, st, &mut p);
        return p.finish(len);
    }

    // Sequence and complex-key markers, which may stack: `- - a`.
    let mut i = indent;
    let mut node_col = indent;
    while i < len && (b[i] == b'-' || b[i] == b'?') && (i + 1 == len || b[i + 1] == b' ') {
        p.tok(i, i + 1, TokenRole::Punctuation);
        node_col = i;
        i += 1;
        while i < len && b[i] == b' ' {
            i += 1;
        }
    }
    if i >= len {
        return p.finish(len);
    }

    // Properties can sit in front of a key: `&anchor key: value`.
    let mut j = i;
    loop {
        if j < len && (b[j] == b'&' || b[j] == b'!') {
            let e = word_end(b, j);
            let mut after = e;
            while after < len && b[after] == b' ' {
                after += 1;
            }
            // Only a property in front of a key belongs to the key; otherwise `value`
            // reads it as the value's own.
            if find_key(b, after, len).is_some() {
                p.tok(j, e, TokenRole::Attribute);
                j = after;
                continue;
            }
        }
        break;
    }

    if let Some((key_end, colon)) = find_key(b, j, len) {
        p.tok(j, key_end, TokenRole::Key);
        p.tok(colon, colon + 1, TokenRole::Punctuation);
        value(b, colon + 1, j, st, &mut p);
    } else {
        value(b, i, node_col, st, &mut p);
    }
    p.finish(len)
}

/// Colours a value starting at `from`. `node_col` is where the node that owns it begins, which
/// is what a block scalar's body has to be indented past.
fn value(b: &[u8], from: usize, node_col: usize, st: &mut State, p: &mut Painter) {
    let len = b.len();
    let mut i = skip_spaces(b, from);

    // Properties on the value itself.
    while i < len && (b[i] == b'&' || b[i] == b'!') {
        let e = word_end(b, i);
        p.tok(i, e, TokenRole::Attribute);
        i = skip_spaces(b, e);
    }
    if i >= len {
        return;
    }

    match b[i] {
        b'#' => p.tok(i, len, TokenRole::Comment),
        b'*' => {
            let e = word_end(b, i);
            p.tok(i, e, TokenRole::Name);
            trailer(b, e, p);
        }
        b'|' | b'>' => {
            let mut j = i + 1;
            while j < len && (b[j].is_ascii_digit() || b[j] == b'+' || b[j] == b'-') {
                j += 1;
            }
            let after = skip_spaces(b, j);
            if after == len || b[after] == b'#' {
                p.tok(i, j, TokenRole::Punctuation);
                st.block_parent = Some(node_col);
                trailer(b, j, p);
            } else {
                plain(b, i, len, p);
            }
        }
        q @ (b'"' | b'\'') => match closing_quote(b, i + 1, q) {
            Some(end) => {
                p.tok(i, end, TokenRole::String);
                trailer(b, end, p);
            }
            None => {
                p.tok(i, len, TokenRole::String);
                st.open_quote = Some(q);
            }
        },
        b'[' | b'{' => flow(b, i, p),
        _ => plain(b, i, len, p),
    }
}

/// A plain scalar from `from`, stopping at a comment, then the comment.
fn plain(b: &[u8], from: usize, limit: usize, p: &mut Painter) {
    let mut end = limit;
    let mut k = from + 1;
    while k < limit {
        if b[k] == b'#' && b[k - 1] == b' ' {
            end = k;
            break;
        }
        k += 1;
    }
    let text_end = trim_end(b, from, end);
    p.tok(from, text_end, classify(&b[from..text_end]));
    if end < limit {
        p.tok(end, limit, TokenRole::Comment);
    }
}

/// After a value: spaces and, if there is one, a comment.
fn trailer(b: &[u8], from: usize, p: &mut Painter) {
    let i = skip_spaces(b, from);
    if i < b.len() && b[i] == b'#' {
        p.tok(i, b.len(), TokenRole::Comment);
    }
}

/// A flow collection (`[a, b]`, `{k: v}`) on one line.
fn flow(b: &[u8], from: usize, p: &mut Painter) {
    let len = b.len();
    let mut seg = from;
    let mut j = from;
    while j < len {
        match b[j] {
            b'[' | b']' | b'{' | b'}' | b',' => {
                flow_item(b, seg, j, p);
                p.tok(j, j + 1, TokenRole::Punctuation);
                seg = j + 1;
            }
            q @ (b'"' | b'\'') if skip_spaces(b, seg) == j => {
                // A quoted item is skipped whole, so punctuation inside it splits nothing.
                match closing_quote(b, j + 1, q) {
                    Some(end) => j = end - 1,
                    None => j = len - 1,
                }
            }
            b'#' if j > from && b[j - 1] == b' ' => {
                flow_item(b, seg, j, p);
                p.tok(j, len, TokenRole::Comment);
                return;
            }
            _ => {}
        }
        j += 1;
    }
    flow_item(b, seg, len, p);
}

/// One item between flow punctuation: `key: value`, or a bare scalar.
fn flow_item(b: &[u8], from: usize, to: usize, p: &mut Painter) {
    let start = skip_spaces(b, from);
    let end = trim_end(b, start, to);
    if start >= end {
        return;
    }
    if let Some((key_end, colon)) = find_key(b, start, end) {
        p.tok(start, key_end, TokenRole::Key);
        p.tok(colon, colon + 1, TokenRole::Punctuation);
        let v = skip_spaces(b, colon + 1);
        scalar(b, v, end, p);
    } else {
        scalar(b, start, end, p);
    }
}

/// A scalar already bounded on both sides: no comment or block handling.
fn scalar(b: &[u8], from: usize, to: usize, p: &mut Painter) {
    let from = skip_spaces(b, from);
    let to = trim_end(b, from, to);
    if from >= to {
        return;
    }
    match b[from] {
        b'*' => p.tok(from, to, TokenRole::Name),
        b'"' | b'\'' => p.tok(from, to, TokenRole::String),
        _ => p.tok(from, to, classify(&b[from..to])),
    }
}

/// Finds `key:` at `from`, within `..end`. Returns where the key text ends (before any
/// spaces ahead of the colon) and where the colon is.
///
/// A colon only makes a key when a space or the end of the line follows it: `http://x` is a
/// plain scalar, `a:1` is too.
fn find_key(b: &[u8], from: usize, end: usize) -> Option<(usize, usize)> {
    if from >= end {
        return None;
    }
    match b[from] {
        b'[' | b'{' | b'#' | b'|' | b'>' | b'*' => None,
        q @ (b'"' | b'\'') => {
            let after = closing_quote(b, from + 1, q)?;
            let colon = skip_spaces(b, after);
            (colon < end && b[colon] == b':' && (colon + 1 == end || b[colon + 1] == b' '))
                .then_some((after, colon))
        }
        _ => {
            let mut k = from;
            while k < end {
                if b[k] == b':' && (k + 1 == end || b[k + 1] == b' ') {
                    let key_end = trim_end(b, from, k);
                    return (key_end > from).then_some((key_end, k));
                }
                if b[k] == b'#' && k > from && b[k - 1] == b' ' {
                    return None;
                }
                k += 1;
            }
            None
        }
    }
}

/// The index just past the quote that closes a scalar opened with `q`, searching from `from`.
fn closing_quote(b: &[u8], from: usize, q: u8) -> Option<usize> {
    let mut k = from;
    while k < b.len() {
        if q == b'"' && b[k] == b'\\' {
            k += 2;
            continue;
        }
        if b[k] == q {
            // `''` inside a single-quoted scalar is an escaped quote, not the end.
            if q == b'\'' && b.get(k + 1) == Some(&b'\'') {
                k += 2;
                continue;
            }
            return Some(k + 1);
        }
        k += 1;
    }
    None
}

fn skip_spaces(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    i
}

/// `end` moved back over trailing spaces, but never before `from`.
fn trim_end(b: &[u8], from: usize, mut end: usize) -> usize {
    while end > from && (b[end - 1] == b' ' || b[end - 1] == b'\t') {
        end -= 1;
    }
    end
}

/// The end of an anchor, alias or tag: up to a space or a flow indicator.
fn word_end(b: &[u8], from: usize) -> usize {
    let mut k = from + 1;
    while k < b.len() && !matches!(b[k], b' ' | b'\t' | b',' | b']' | b'}') {
        k += 1;
    }
    k
}

/// What a plain scalar is, by the YAML 1.2 core schema. `yes`, `no`, `on` and `off` are
/// strings there, and colouring them as booleans would lie about what a 1.2 parser reads.
fn classify(text: &[u8]) -> TokenRole {
    let Ok(s) = std::str::from_utf8(text) else {
        return TokenRole::String;
    };
    match s {
        "" | "~" | "null" | "Null" | "NULL" => TokenRole::Null,
        "true" | "True" | "TRUE" | "false" | "False" | "FALSE" => TokenRole::Bool,
        _ if is_number(s) => TokenRole::Number,
        _ => TokenRole::String,
    }
}

fn is_number(s: &str) -> bool {
    let unsigned = s.strip_prefix(['+', '-']).unwrap_or(s);
    if matches!(
        unsigned,
        ".inf" | ".Inf" | ".INF" | ".nan" | ".NaN" | ".NAN"
    ) {
        return true;
    }
    if let Some(hex) = s.strip_prefix("0x") {
        return !hex.is_empty() && hex.bytes().all(|c| c.is_ascii_hexdigit());
    }
    if let Some(oct) = s.strip_prefix("0o") {
        return !oct.is_empty() && oct.bytes().all(|c| (b'0'..=b'7').contains(&c));
    }
    // `parse::<f64>` alone would accept `inf` and `infinity`, which are strings in YAML.
    unsigned
        .bytes()
        .all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-'))
        && unsigned.bytes().any(|c| c.is_ascii_digit())
        && s.parse::<f64>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_key_and_its_scalar_get_different_roles() {
        assert_eq!(
            one("name: termdoc"),
            "{Key|name}{Punctuation|:} {String|termdoc}"
        );
    }

    #[test]
    fn scalars_are_classified_by_the_core_schema() {
        assert_eq!(one("a: 1.5"), "{Key|a}{Punctuation|:} {Number|1.5}");
        assert_eq!(one("a: true"), "{Key|a}{Punctuation|:} {Bool|true}");
        assert_eq!(one("a: ~"), "{Key|a}{Punctuation|:} {Null|~}");
        assert_eq!(one("a: 0x1F"), "{Key|a}{Punctuation|:} {Number|0x1F}");
        // YAML 1.2: these are strings, and calling them booleans would be a lie.
        assert_eq!(one("a: yes"), "{Key|a}{Punctuation|:} {String|yes}");
        assert_eq!(one("a: inf"), "{Key|a}{Punctuation|:} {String|inf}");
        assert_eq!(one("a: 1.2.3"), "{Key|a}{Punctuation|:} {String|1.2.3}");
    }

    #[test]
    fn a_comment_is_kept_and_is_not_part_of_the_value() {
        assert_eq!(
            one("port: 80 # http"),
            "{Key|port}{Punctuation|:} {Number|80} {Comment|# http}"
        );
        assert_eq!(one("# only a comment"), "{Comment|# only a comment}");
        assert_eq!(one("  # indented"), "  {Comment|# indented}");
    }

    #[test]
    fn a_hash_inside_a_scalar_is_not_a_comment() {
        assert_eq!(
            one("color: red#1"),
            "{Key|color}{Punctuation|:} {String|red#1}"
        );
        assert_eq!(
            one(r##"c: "a # b""##),
            r##"{Key|c}{Punctuation|:} {String|"a # b"}"##
        );
    }

    #[test]
    fn a_colon_without_a_space_does_not_make_a_key() {
        assert_eq!(one("http://example.com"), "{String|http://example.com}");
        assert_eq!(
            one("url: http://example.com"),
            "{Key|url}{Punctuation|:} {String|http://example.com}"
        );
    }

    #[test]
    fn sequence_markers_may_stack_and_lead_a_mapping() {
        assert_eq!(one("- a"), "{Punctuation|-} {String|a}");
        assert_eq!(one("- - a"), "{Punctuation|-} {Punctuation|-} {String|a}");
        assert_eq!(
            one("  - name: x"),
            "  {Punctuation|-} {Key|name}{Punctuation|:} {String|x}"
        );
    }

    #[test]
    fn quoted_keys_and_values() {
        assert_eq!(
            one(r#""a b": 'c'"#),
            r#"{Key|"a b"}{Punctuation|:} {String|'c'}"#
        );
        assert_eq!(one("a: 'it''s'"), "{Key|a}{Punctuation|:} {String|'it''s'}");
    }

    #[test]
    fn anchors_aliases_and_tags() {
        assert_eq!(one("base: &b"), "{Key|base}{Punctuation|:} {Attribute|&b}");
        assert_eq!(one("other: *b"), "{Key|other}{Punctuation|:} {Name|*b}");
        assert_eq!(
            one("x: !!str 12"),
            "{Key|x}{Punctuation|:} {Attribute|!!str} {Number|12}"
        );
        assert_eq!(
            one("&k key: v"),
            "{Attribute|&k} {Key|key}{Punctuation|:} {String|v}"
        );
    }

    #[test]
    fn document_markers_and_directives() {
        assert_eq!(one("---"), "{Punctuation|---}");
        assert_eq!(one("..."), "{Punctuation|...}");
        assert_eq!(one("%YAML 1.2"), "{Attribute|%YAML 1.2}");
        // Three dashes with text after them are a scalar, not a marker.
        assert_eq!(one("---x"), "{String|---x}");
    }

    #[test]
    fn a_block_scalar_body_is_a_string_until_the_indent_returns() {
        let mut st = State::default();
        assert_eq!(
            show("run: |", &mut st),
            "{Key|run}{Punctuation|:} {Punctuation||}"
        );
        // What would be a key anywhere else is text here.
        assert_eq!(show("  echo: hi", &mut st), "  {String|echo: hi}");
        assert_eq!(show("", &mut st), "");
        assert_eq!(show("  more", &mut st), "  {String|more}");
        assert_eq!(
            show("next: 1", &mut st),
            "{Key|next}{Punctuation|:} {Number|1}"
        );
    }

    #[test]
    fn a_block_scalar_in_a_sequence_item_is_indented_past_the_key() {
        let mut st = State::default();
        show("- run: >-", &mut st);
        // At the dash's column plus two this is a sibling key, not the body.
        assert_eq!(
            show("  other: 1", &mut st),
            "  {Key|other}{Punctuation|:} {Number|1}"
        );
    }

    #[test]
    fn a_quoted_scalar_can_span_lines() {
        let mut st = State::default();
        assert_eq!(
            show(r#"msg: "first"#, &mut st),
            r#"{Key|msg}{Punctuation|:} {String|"first}"#
        );
        assert!(st.open_quote.is_some());
        assert_eq!(
            show(r#"  second" # done"#, &mut st),
            r##"  {String|second"} {Comment|# done}"##
        );
        assert!(st.open_quote.is_none());
    }

    #[test]
    fn flow_collections_on_one_line() {
        assert_eq!(
            one("t: [a, 1, true]"),
            "{Key|t}{Punctuation|:} {Punctuation|[}{String|a}{Punctuation|,} \
             {Number|1}{Punctuation|,} {Bool|true}{Punctuation|]}"
        );
        assert_eq!(
            one("m: {k: v}"),
            "{Key|m}{Punctuation|:} {Punctuation|{}{Key|k}{Punctuation|:} {String|v}\
             {Punctuation|}}"
        );
        // Punctuation inside a quoted item does not split it.
        assert_eq!(
            one(r#"t: ["a, b"]"#),
            r#"{Key|t}{Punctuation|:} {Punctuation|[}{String|"a, b"}{Punctuation|]}"#
        );
    }

    #[test]
    fn the_pieces_always_cover_the_whole_line() {
        // Whatever the colouring decides, no byte may be lost: this is a viewer, and a lost
        // character is the one failure it cannot have.
        for line in [
            "",
            "   ",
            "a: b # c",
            "- - ? x",
            "k: [1, {a: 'b'}, *c] # t",
            "\tkey:\tvalue",
            "é: ü",
            "'unterminated",
            "a: |-",
            "--- !tag &a text",
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
            Box::leak(Box::new(Source::from_bytes("t.yaml", bytes.to_vec())));
        YamlReader::new()
            .read(src, &ReadContext::default())
            .unwrap()
            .map(|e| e.unwrap().node)
            .collect()
    }

    #[test]
    fn the_text_survives_byte_for_byte() {
        let input = "# c\nname: x # y\nlist:\n  - 1\n  - [a, b]\nblock: |\n  keep: this\n";
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
    fn the_stream_is_well_formed_and_balanced() {
        let ev = events(b"a: 1\nb: [x]\n");
        assert!(matches!(ev.first(), Some(Event::Start(Tag::Document(_)))));
        assert!(matches!(ev.last(), Some(Event::End(TagKind::Document))));
        let opens = ev.iter().filter(|e| matches!(e, Event::Start(_))).count();
        let closes = ev.iter().filter(|e| matches!(e, Event::End(_))).count();
        assert_eq!(opens, closes);
    }

    #[test]
    fn valid_text_is_never_copied() {
        // The memory invariant: borrowed in, borrowed out.
        for e in events(b"a: 1 # c\n- b\n") {
            if let Event::Text(t) = e {
                assert!(matches!(t, Cow::Borrowed(_)), "'{t}' was copied");
            }
        }
    }

    #[test]
    fn an_empty_file_is_an_empty_document() {
        let ev = events(b"");
        assert!(!ev.iter().any(|e| matches!(e, Event::Text(_))));
        assert!(matches!(ev.last(), Some(Event::End(TagKind::Document))));
    }

    #[test]
    fn capabilities_claim_streaming_because_it_streams() {
        assert!(YamlReader::new().capabilities().streaming);
    }
}
