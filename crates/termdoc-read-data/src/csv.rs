//! The CSV reader: a delimited file as a table.
//!
//! # What it emits
//!
//! `Tag::Table`, with the first record as the header (`TableHead`) and every other record as a
//! row. The layout already knows how to allocate widths, wrap long cells and degrade the grid
//! to ASCII and then to TSV (docs/DESIGN.md §5), so none of that is repeated here. A column
//! whose sampled cells are all numbers is right-aligned.
//!
//! The first record is *always* the header. Guessing whether a file has one is a heuristic that
//! is wrong in both directions, and a header row that renders as a body row is the lesser
//! surprise than the reverse.
//!
//! # The delimiter
//!
//! It arrives in [`ReadContext::delimiter`], resolved by `termdoc-detect` (comma, semicolon,
//! tab or pipe). A reader cannot call detection itself — it sees `termdoc-core` and nothing
//! else — so `run.rs` carries the answer across. Without one, a comma.
//!
//! # Why a ceiling
//!
//! The layout's table builder holds the whole table to allocate column widths: it cannot know
//! how wide column 3 needs to be until it has seen every row. That is the one place in the
//! pipeline that is not streaming, so it needs a bound, and [`MAX_TABLE_CELLS`] is it.
//!
//! The bound is in **cells**, not rows, because that is what the memory follows: a wrap buffer
//! per cell, and then a laid-out line per cell. A row ceiling would let a 40-column file cost
//! eight times what a 5-column one does.
//!
//! Past the ceiling nothing is refused or dropped. The table closes, a warning says so, and
//! **the remaining records follow verbatim** as preformatted text, read lazily and borrowed from
//! the source, so `termdoc big.csv | head` still reads a few pages and a ten-million-row file
//! still costs a few megabytes. The same trade as the JSON reader's ceiling
//! (`json.rs`), and for the same reason: an oversized document degrades, it does not vanish.
//!
//! # Parsing
//!
//! A small RFC 4180 parser over the bytes: quoted fields, `""` for a quote, delimiters and
//! newlines inside quotes, CRLF, blank lines skipped. Fields borrow from the source unless they
//! need unescaping. It is lenient on purpose — a viewer does not reject a ragged row or a stray
//! quote — and unquoted fields are trimmed of surrounding blanks, which RFC 4180 would keep but
//! a table does not want. A newline inside a cell is shown as a space: a table cell is one
//! line of text until the layout wraps it.
//!
//! The first non-blank line of an unterminated quote swallows the rest of the file into one
//! field, as every CSV parser does; the reader warns once when that happens.

use std::borrow::Cow;
use std::collections::VecDeque;

use termdoc_core::{
    Align, Diagnostic, DocumentReader, Event, Events, FormatId, Metadata, ReadContext, ReaderCaps,
    Result, Source, Span, Spanned, Tag, TagKind,
};

/// How many cells become the table before the rest is shown verbatim. The table gets
/// `MAX_TABLE_CELLS / columns` rows, and at least one.
///
/// **Measured on the release binary** (`/usr/bin/time -l`, peak memory footprint, a 5-column
/// file of short words and numbers, laid out at 100 columns):
///
/// | rows | cells | process own memory | per cell |
/// |---|---:|---:|---:|
/// | 1,000 | 5,000 | 6.1 MB | ~0.9 KB |
/// | 5,000 | 25,000 | 24.1 MB | ~0.9 KB |
/// | 20,000 | 100,000 | 90.8 MB | ~0.9 KB |
/// | 100,000 | 500,000 | 447.6 MB | ~0.9 KB |
///
/// The cost is linear and about a kilobyte a cell, so the ceiling is 25,000 cells: ~24 MB, half
/// the 50 MB budget (docs/DESIGN.md §8), which leaves room for wider cells than the sample's.
/// That is 5,000 rows of 5 columns, and 1,250 rows of 20. As with JSON, measure the whole
/// pipeline, not the part: the reader itself costs almost nothing here.
pub const MAX_TABLE_CELLS: usize = 25_000;

/// How many records are read ahead to decide which columns are numeric.
const ALIGN_SAMPLE: usize = 64;

#[derive(Debug, Clone, Copy)]
pub struct CsvReader {
    max_cells: usize,
}

impl Default for CsvReader {
    fn default() -> Self {
        CsvReader {
            max_cells: MAX_TABLE_CELLS,
        }
    }
}

impl CsvReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// A reader with a different ceiling, for tests that should not need thousands of rows.
    pub fn with_max_cells(max_cells: usize) -> Self {
        CsvReader { max_cells }
    }
}

impl DocumentReader for CsvReader {
    fn id(&self) -> FormatId {
        FormatId::Csv
    }

    fn capabilities(&self) -> ReaderCaps {
        ReaderCaps {
            // True of the reader: records are pulled on demand. The *layout* buffers the
            // table, which is what the ceiling is for.
            streaming: true,
            paginated: false,
            metadata: false,
        }
    }

    fn read<'a>(&self, src: &'a Source, ctx: &ReadContext) -> Result<Events<'a>> {
        Ok(Box::new(CsvEvents::new(
            src,
            ctx.delimiter.unwrap_or(b','),
            self.max_cells,
        )))
    }
}

// ---------------------------------------------------------------------------- parsing

/// The bytes of one field: borrowed from the source unless unescaping made a copy.
enum Raw<'a> {
    Slice(&'a [u8]),
    Owned(Vec<u8>),
}

impl Raw<'_> {
    fn bytes(&self) -> &[u8] {
        match self {
            Raw::Slice(s) => s,
            Raw::Owned(v) => v,
        }
    }
}

/// Where the parser stands. Cheap to copy, which is what lets the reader look ahead without
/// disturbing the real position.
#[derive(Clone, Copy)]
struct Cursor<'a> {
    hay: &'a [u8],
    pos: usize,
    delim: u8,
}

/// What ended a field.
enum End {
    Delimiter,
    Line,
}

impl<'a> Cursor<'a> {
    fn at_end(&self) -> bool {
        self.pos >= self.hay.len()
    }

    /// The next non-blank record, or `None` at the end. The flag says a quote was never
    /// closed.
    fn record(&mut self) -> Option<(Vec<Raw<'a>>, bool)> {
        // Blank lines are separators, not empty records.
        while self.pos < self.hay.len() {
            match self.hay[self.pos] {
                b'\n' => self.pos += 1,
                b'\r' if self.hay.get(self.pos + 1) == Some(&b'\n') => self.pos += 2,
                _ => break,
            }
        }
        if self.at_end() {
            return None;
        }

        let mut fields = Vec::new();
        let mut unterminated = false;
        loop {
            let (field, end, open) = self.field();
            unterminated |= open;
            fields.push(field);
            if matches!(end, End::Line) {
                break;
            }
        }
        Some((fields, unterminated))
    }

    /// One field, consuming its delimiter or line end.
    fn field(&mut self) -> (Raw<'a>, End, bool) {
        let b = self.hay;
        let len = b.len();
        let start = self.pos;

        let mut j = start;
        while j < len && b[j] == b' ' {
            j += 1;
        }
        if j < len && b[j] == b'"' {
            return self.quoted(j + 1);
        }

        let mut m = start;
        while m < len && b[m] != self.delim && b[m] != b'\n' {
            m += 1;
        }
        let end = self.finish(m);
        let raw = trim_blanks(&b[start..m], self.delim);
        (Raw::Slice(raw), end, false)
    }

    /// The body of a quoted field, from just after the opening quote.
    fn quoted(&mut self, from: usize) -> (Raw<'a>, End, bool) {
        let b = self.hay;
        let len = b.len();
        let mut k = from;
        let mut escaped = false;
        let close = loop {
            match b[k..].iter().position(|c| *c == b'"') {
                None => {
                    // Never closed: everything left is this field.
                    self.pos = len;
                    return (Raw::Slice(&b[from..]), End::Line, true);
                }
                Some(off) => {
                    let q = k + off;
                    if b.get(q + 1) == Some(&b'"') {
                        escaped = true;
                        k = q + 2;
                    } else {
                        break q;
                    }
                }
            }
        };

        // Anything between the closing quote and the delimiter is junk. Blanks are dropped;
        // anything else is kept, because a viewer does not silently lose text.
        let mut m = close + 1;
        while m < len && b[m] != self.delim && b[m] != b'\n' {
            m += 1;
        }
        let tail = trim_blanks(&b[close + 1..m], self.delim);
        let end = self.finish(m);

        let body = &b[from..close];
        if !escaped && tail.is_empty() {
            return (Raw::Slice(body), end, false);
        }
        let mut owned = Vec::with_capacity(body.len() + tail.len());
        let mut i = 0;
        while i < body.len() {
            owned.push(body[i]);
            // `""` is one quote.
            i += if body[i] == b'"' { 2 } else { 1 };
        }
        owned.extend_from_slice(tail);
        (Raw::Owned(owned), end, false)
    }

    /// Consumes the delimiter or line end at `at` and says which it was.
    fn finish(&mut self, at: usize) -> End {
        if at >= self.hay.len() {
            self.pos = self.hay.len();
            return End::Line;
        }
        self.pos = at + 1;
        if self.hay[at] == self.delim {
            End::Delimiter
        } else {
            End::Line
        }
    }
}

/// `field` without the blanks around it — and the `\r` of a CRLF — but never the delimiter,
/// which matters when the delimiter is a tab.
fn trim_blanks(mut field: &[u8], delim: u8) -> &[u8] {
    let blank = |c: u8| c == b' ' || c == b'\r' || (c == b'\t' && delim != b'\t');
    while let [first, rest @ ..] = field {
        if blank(*first) {
            field = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = field {
        if blank(*last) {
            field = rest;
        } else {
            break;
        }
    }
    field
}

/// Whether a cell reads as a number, for right-aligning its column.
fn looks_numeric(cell: &[u8]) -> bool {
    let Ok(s) = std::str::from_utf8(cell) else {
        return false;
    };
    // `parse::<f64>` alone would accept `inf`, `nan` and `infinity`, which are words.
    s.bytes()
        .next()
        .is_some_and(|c| c.is_ascii_digit() || matches!(c, b'+' | b'-' | b'.'))
        && s.parse::<f64>().is_ok()
}

// ---------------------------------------------------------------------------- events

#[derive(Debug, PartialEq, Eq)]
enum Phase {
    Header,
    Rows,
    /// Past the ceiling: the remaining records, as they are.
    Verbatim,
    Done,
}

struct CsvEvents<'a> {
    cur: Cursor<'a>,
    /// Whether `hay` is the source's own bytes (which may hold invalid UTF-8) or text that was
    /// already decoded.
    raw: bool,
    name: String,
    /// The cell ceiling, and the row ceiling derived from it once the column count is known.
    max_cells: usize,
    max_rows: usize,
    rows: usize,
    phase: Phase,
    pending: VecDeque<Spanned<Event<'a>>>,
    warned_encoding: bool,
    warned_quote: bool,
}

impl<'a> CsvEvents<'a> {
    fn new(src: &'a Source, delim: u8, max_cells: usize) -> Self {
        // UTF-8 is read in place, so nothing walks the file up front; anything else is
        // transcoded once by `as_str`, which caches it in the source.
        let utf8 = src.encoding_name().eq_ignore_ascii_case("utf-8");
        let hay: &'a [u8] = if utf8 {
            src.bytes()
        } else {
            src.as_str().0.as_bytes()
        };
        // A byte-order mark is not part of the first header.
        let start = if hay.starts_with(&[0xEF, 0xBB, 0xBF]) {
            3
        } else {
            0
        };
        CsvEvents {
            cur: Cursor {
                hay,
                pos: start,
                delim,
            },
            raw: utf8,
            name: src.display_name().to_string(),
            max_cells,
            max_rows: usize::MAX,
            rows: 0,
            phase: Phase::Header,
            pending: VecDeque::new(),
            warned_encoding: false,
            warned_quote: false,
        }
    }

    fn push(&mut self, event: Event<'a>, from: usize, to: usize) {
        self.pending
            .push_back(Spanned::new(event, Span::bytes(from as u64, to as u64)));
    }

    fn bare(&mut self, event: Event<'a>) {
        self.pending.push_back(Spanned::bare(event));
    }

    fn warn(&mut self, message: String, at: usize) {
        self.push(Event::Diagnostic(Diagnostic::warning(message)), at, at);
    }

    /// A field as text. A newline inside it becomes a space, and invalid UTF-8 is replaced,
    /// with one warning.
    fn text(&mut self, raw: &Raw<'a>, at: usize) -> Cow<'a, str> {
        let bytes = raw.bytes();
        let decoded: Cow<'a, str> = match raw {
            Raw::Slice(s) => match std::str::from_utf8(s) {
                Ok(t) => Cow::Borrowed(t),
                Err(_) => Cow::Owned(String::from_utf8_lossy(s).into_owned()),
            },
            Raw::Owned(v) => Cow::Owned(String::from_utf8_lossy(v).into_owned()),
        };
        if !self.raw {
            debug_assert!(std::str::from_utf8(bytes).is_ok());
        } else if !self.warned_encoding && std::str::from_utf8(bytes).is_err() {
            self.warned_encoding = true;
            self.warn(
                format!("{} is not valid UTF-8; shown with replacements", self.name),
                at,
            );
        }
        if decoded.contains(['\n', '\r']) {
            // `\r\n` is one break, so one space.
            let flat = decoded.replace("\r\n", " ").replace(['\n', '\r'], " ");
            return Cow::Owned(flat);
        }
        decoded
    }

    fn cell(&mut self, raw: &Raw<'a>, at: usize) {
        let text = self.text(raw, at);
        self.bare(Event::Start(Tag::TableCell {
            colspan: 1,
            rowspan: 1,
        }));
        if !text.is_empty() {
            self.bare(Event::Text(text));
        }
        self.bare(Event::End(TagKind::TableCell));
    }

    fn note_quote(&mut self, unterminated: bool, at: usize) {
        if unterminated && !self.warned_quote {
            self.warned_quote = true;
            self.warn(
                format!(
                    "{} has a quoted field that is never closed; the rest of the file is \
                     part of it",
                    self.name
                ),
                at,
            );
        }
    }

    fn fill(&mut self) {
        match self.phase {
            Phase::Header => self.fill_header(),
            Phase::Rows => self.fill_row(),
            Phase::Verbatim => self.fill_verbatim(),
            Phase::Done => {}
        }
    }

    fn fill_header(&mut self) {
        self.bare(Event::Start(Tag::Document(Box::new(Metadata {
            source_format: Some(FormatId::Csv),
            ..Metadata::default()
        }))));

        let start = self.cur.pos;
        let Some((header, unterminated)) = self.cur.record() else {
            self.bare(Event::End(TagKind::Document));
            self.phase = Phase::Done;
            return;
        };
        self.note_quote(unterminated, start);
        self.max_rows = (self.max_cells / header.len().max(1)).max(1);

        // Which columns are numbers, from a bounded look ahead on a copy of the cursor.
        let mut numeric = vec![true; header.len()];
        let mut seen = vec![false; header.len()];
        let mut ahead = self.cur;
        for _ in 0..ALIGN_SAMPLE {
            let Some((record, _)) = ahead.record() else {
                break;
            };
            for (col, field) in record.iter().take(header.len()).enumerate() {
                let cell = field.bytes();
                if !cell.is_empty() {
                    seen[col] = true;
                    numeric[col] &= looks_numeric(cell);
                }
            }
        }
        let align = (0..header.len())
            .map(|c| {
                if seen[c] && numeric[c] {
                    Align::Right
                } else {
                    Align::None
                }
            })
            .collect();

        self.bare(Event::Start(Tag::Table { align }));
        self.bare(Event::Start(Tag::TableHead));
        for field in &header {
            self.cell(field, start);
        }
        self.bare(Event::End(TagKind::TableHead));
        self.phase = Phase::Rows;
    }

    fn fill_row(&mut self) {
        if self.rows >= self.max_rows {
            // At the ceiling: is there anything left to say it about?
            let mut ahead = self.cur;
            if ahead.record().is_none() {
                return self.finish();
            }
            let at = self.cur.pos;
            self.bare(Event::End(TagKind::Table));
            self.warn(
                format!(
                    "{} has more than {} rows; the first {} are shown as a table and the rest \
                     verbatim",
                    self.name, self.max_rows, self.max_rows
                ),
                at,
            );
            self.bare(Event::Start(Tag::Preformatted));
            self.phase = Phase::Verbatim;
            return;
        }

        let start = self.cur.pos;
        let Some((record, unterminated)) = self.cur.record() else {
            return self.finish();
        };
        self.note_quote(unterminated, start);
        self.rows += 1;
        self.bare(Event::Start(Tag::TableRow));
        for field in &record {
            self.cell(field, start);
        }
        self.bare(Event::End(TagKind::TableRow));
    }

    fn fill_verbatim(&mut self) {
        let hay = self.cur.hay;
        if self.cur.at_end() {
            self.bare(Event::End(TagKind::Preformatted));
            self.bare(Event::End(TagKind::Document));
            self.phase = Phase::Done;
            return;
        }
        let start = self.cur.pos;
        // The terminator stays on the line: inside `Preformatted` it is what closes it.
        let end = hay[start..]
            .iter()
            .position(|b| *b == b'\n')
            .map_or(hay.len(), |nl| start + nl + 1);
        self.cur.pos = end;
        let text = self.text(&Raw::Slice(&hay[start..end]), start);
        // `text` flattens newlines for cells; a verbatim line must keep its own.
        let text = match text {
            Cow::Owned(_) => Cow::Owned(String::from_utf8_lossy(&hay[start..end]).into_owned()),
            borrowed => borrowed,
        };
        self.push(Event::Text(text), start, end);
    }

    fn finish(&mut self) {
        self.bare(Event::End(TagKind::Table));
        self.bare(Event::End(TagKind::Document));
        self.phase = Phase::Done;
    }
}

impl<'a> Iterator for CsvEvents<'a> {
    type Item = Result<Spanned<Event<'a>>>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(Ok(event));
            }
            if self.phase == Phase::Done {
                return None;
            }
            self.fill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn records(input: &str, delim: u8) -> Vec<Vec<String>> {
        let mut cur = Cursor {
            hay: input.as_bytes(),
            pos: 0,
            delim,
        };
        let mut out = Vec::new();
        while let Some((rec, _)) = cur.record() {
            out.push(
                rec.iter()
                    .map(|f| String::from_utf8_lossy(f.bytes()).into_owned())
                    .collect(),
            );
        }
        out
    }

    fn csv(input: &str) -> Vec<Vec<String>> {
        records(input, b',')
    }

    #[test]
    fn plain_records() {
        assert_eq!(csv("a,b,c\n1,2,3\n"), [["a", "b", "c"], ["1", "2", "3"]]);
    }

    #[test]
    fn quoted_fields_hold_delimiters_quotes_and_newlines() {
        assert_eq!(csv("\"a,b\",c\n"), [["a,b", "c"]]);
        assert_eq!(csv("\"say \"\"hi\"\"\",x\n"), [["say \"hi\"", "x"]]);
        assert_eq!(csv("\"two\nlines\",x\n"), [["two\nlines", "x"]]);
    }

    #[test]
    fn crlf_blank_lines_and_a_missing_final_newline() {
        assert_eq!(csv("a,b\r\n\r\n1,2"), [["a", "b"], ["1", "2"]]);
        assert_eq!(csv("\n\na,b\n\n\n"), [["a", "b"]]);
    }

    #[test]
    fn empty_fields_are_kept_and_blanks_are_trimmed() {
        assert_eq!(csv("a,,c\n"), [["a", "", "c"]]);
        assert_eq!(csv(",\n"), [["", ""]]);
        assert_eq!(csv("a , b ,c\n"), [["a", "b", "c"]]);
        assert_eq!(csv("a,\n"), [["a", ""]]);
    }

    #[test]
    fn other_delimiters() {
        assert_eq!(records("a;b\n1;2\n", b';'), [["a", "b"], ["1", "2"]]);
        // A tab delimiter must survive the trimming of blanks.
        assert_eq!(records("a\t\tc\n", b'\t'), [["a", "", "c"]]);
        assert_eq!(records("a|b\n", b'|'), [["a", "b"]]);
    }

    #[test]
    fn text_after_a_closing_quote_is_kept() {
        // Not valid CSV, but a viewer does not drop characters.
        assert_eq!(csv("\"a\"b,c\n"), [["ab", "c"]]);
        assert_eq!(csv("\"a\"  ,c\n"), [["a", "c"]]);
    }

    #[test]
    fn an_unterminated_quote_takes_the_rest_and_says_so() {
        let mut cur = Cursor {
            hay: b"a,\"never\nclosed,x\n",
            pos: 0,
            delim: b',',
        };
        let (rec, open) = cur.record().unwrap();
        assert!(open);
        assert_eq!(rec.len(), 2);
        assert!(cur.at_end());
    }

    #[test]
    fn unquoted_and_unescaped_fields_borrow() {
        let mut cur = Cursor {
            hay: b"a,\"b\",\"c\"\"d\"\n",
            pos: 0,
            delim: b',',
        };
        let (rec, _) = cur.record().unwrap();
        assert!(matches!(rec[0], Raw::Slice(_)));
        assert!(matches!(rec[1], Raw::Slice(_)));
        // Only a field that had to be unescaped is copied.
        assert!(matches!(rec[2], Raw::Owned(_)));
    }

    #[test]
    fn numbers_are_numbers_and_words_are_not() {
        for yes in ["1", "-2", "3.5", "+4", ".5", "1e3", "-0.25"] {
            assert!(looks_numeric(yes.as_bytes()), "{yes}");
        }
        for no in ["", "a", "inf", "nan", "infinity", "1a", "12-34", "-", "."] {
            assert!(!looks_numeric(no.as_bytes()), "{no}");
        }
    }

    // ---- the reader

    fn read(input: &[u8], delim: u8, reader: CsvReader) -> Vec<Event<'static>> {
        // Leaked on purpose: the events borrow the source, and a test does not care.
        let src: &'static Source = Box::leak(Box::new(Source::from_bytes("t.csv", input.to_vec())));
        let ctx = ReadContext {
            delimiter: Some(delim),
            ..ReadContext::default()
        };
        reader
            .read(src, &ctx)
            .unwrap()
            .map(|e| e.unwrap().node)
            .collect()
    }

    fn table(input: &str) -> Vec<Event<'static>> {
        read(input.as_bytes(), b',', CsvReader::new())
    }

    fn count(events: &[Event<'_>], kind: TagKind) -> usize {
        events
            .iter()
            .filter(|e| matches!(e, Event::Start(t) if t.kind() == kind))
            .count()
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

    fn align_of(events: &[Event<'_>]) -> Vec<Align> {
        events
            .iter()
            .find_map(|e| match e {
                Event::Start(Tag::Table { align }) => Some(align.clone()),
                _ => None,
            })
            .expect("a table")
    }

    #[test]
    fn the_first_record_is_the_header_and_the_rest_are_rows() {
        let ev = table("name,qty\nbolt,3\nnut,5\n");
        assert_eq!(count(&ev, TagKind::Table), 1);
        assert_eq!(count(&ev, TagKind::TableHead), 1);
        assert_eq!(count(&ev, TagKind::TableRow), 2);
        assert_eq!(count(&ev, TagKind::TableCell), 6);
        assert!(matches!(ev.last(), Some(Event::End(TagKind::Document))));
        assert!(warnings(&ev).is_empty());
    }

    #[test]
    fn numeric_columns_are_right_aligned() {
        let ev = table("name,qty,note\nbolt,3,ok\nnut,15,\n");
        assert_eq!(align_of(&ev), [Align::None, Align::Right, Align::None]);
    }

    #[test]
    fn a_column_with_one_word_in_it_is_not_numeric() {
        let ev = table("a,b\n1,x\n2,3\n");
        assert_eq!(align_of(&ev), [Align::Right, Align::None]);
        // Nothing sampled is not numeric either.
        assert_eq!(align_of(&table("a,b\n1,\n")), [Align::Right, Align::None]);
    }

    #[test]
    fn an_empty_file_is_an_empty_document() {
        let ev = table("");
        assert_eq!(count(&ev, TagKind::Table), 0);
        assert!(matches!(ev.last(), Some(Event::End(TagKind::Document))));
    }

    #[test]
    fn a_header_alone_is_a_table_with_no_rows() {
        let ev = table("a,b,c\n");
        assert_eq!(count(&ev, TagKind::TableHead), 1);
        assert_eq!(count(&ev, TagKind::TableRow), 0);
    }

    #[test]
    fn ragged_rows_are_passed_through() {
        let ev = table("a,b,c\n1\n1,2,3,4\n");
        assert_eq!(count(&ev, TagKind::TableRow), 2);
        assert_eq!(count(&ev, TagKind::TableCell), 3 + 1 + 4);
    }

    #[test]
    fn a_byte_order_mark_is_not_part_of_the_first_header() {
        let ev = read(b"\xEF\xBB\xBFname,qty\n1,2\n", b',', CsvReader::new());
        let first = ev
            .iter()
            .find_map(|e| match e {
                Event::Text(t) => Some(t.to_string()),
                _ => None,
            })
            .unwrap();
        assert_eq!(first, "name");
    }

    #[test]
    fn a_newline_inside_a_cell_becomes_a_space() {
        let ev = table("a,b\n\"x\r\ny\",z\n");
        let texts: Vec<String> = ev
            .iter()
            .filter_map(|e| match e {
                Event::Text(t) => Some(t.to_string()),
                _ => None,
            })
            .collect();
        assert!(texts.contains(&"x y".to_string()), "{texts:?}");
    }

    #[test]
    fn cells_are_borrowed_when_they_can_be() {
        for e in table("a,b\n1,\"two\"\n") {
            if let Event::Text(t) = e {
                assert!(matches!(t, Cow::Borrowed(_)), "'{t}' was copied");
            }
        }
    }

    #[test]
    fn past_the_ceiling_the_rest_follows_verbatim_and_nothing_is_lost() {
        let mut input = String::from("n,v\n");
        for i in 0..10 {
            input.push_str(&format!("{i},x{i}\n"));
        }
        let ev = read(input.as_bytes(), b',', CsvReader::with_max_cells(8));

        assert_eq!(count(&ev, TagKind::TableRow), 4);
        let w = warnings(&ev);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("more than 4 rows"), "{}", w[0]);

        // The six records the table did not take, exactly as they were, in order.
        let tail: String = ev
            .iter()
            .skip_while(|e| !matches!(e, Event::Start(Tag::Preformatted)))
            .filter_map(|e| match e {
                Event::Text(t) => Some(t.as_ref()),
                _ => None,
            })
            .collect();
        assert_eq!(tail, "4,x4\n5,x5\n6,x6\n7,x7\n8,x8\n9,x9\n");

        // And the stream is still balanced.
        let opens = ev.iter().filter(|e| matches!(e, Event::Start(_))).count();
        let closes = ev.iter().filter(|e| matches!(e, Event::End(_))).count();
        assert_eq!(opens, closes);
    }

    #[test]
    fn exactly_the_ceiling_is_not_an_overflow() {
        let ev = read(b"n\n1\n2\n3\n", b',', CsvReader::with_max_cells(3));
        assert_eq!(count(&ev, TagKind::TableRow), 3);
        assert!(warnings(&ev).is_empty());
        assert_eq!(count(&ev, TagKind::Preformatted), 0);
    }

    #[test]
    fn an_unterminated_quote_warns_once() {
        let ev = table("a,b\n1,\"open\n2,3\n");
        let w = warnings(&ev);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("never closed"), "{}", w[0]);
    }

    #[test]
    fn invalid_utf8_is_replaced_with_one_warning() {
        let ev = read(b"a,b\n\xFF,\xFE\n", b',', CsvReader::new());
        assert_eq!(warnings(&ev).len(), 1, "{:?}", warnings(&ev));
        assert!(
            ev.iter()
                .any(|e| matches!(e, Event::Text(t) if t.contains('\u{FFFD}')))
        );
    }

    #[test]
    fn a_declared_encoding_is_honored() {
        let mut src = Source::from_bytes("t.csv", b"a,b\nComit\xE9,1\n".to_vec());
        src.set_encoding("windows-1252").unwrap();
        let texts: Vec<String> = CsvReader::new()
            .read(&src, &ReadContext::default())
            .unwrap()
            .filter_map(|e| match e.unwrap().node {
                Event::Text(t) => Some(t.into_owned()),
                _ => None,
            })
            .collect();
        assert!(texts.contains(&"Comité".to_string()), "{texts:?}");
    }

    #[test]
    fn the_delimiter_comes_from_the_context() {
        let ev = read(b"a;b\n1;2\n", b';', CsvReader::new());
        assert_eq!(count(&ev, TagKind::TableCell), 4);
    }

    #[test]
    fn capabilities_claim_streaming_because_records_are_pulled() {
        assert!(CsvReader::new().capabilities().streaming);
    }
}
