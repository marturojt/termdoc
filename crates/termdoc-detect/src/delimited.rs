//! Delimiter detection for CSV and friends.
//!
//! "Are there commas?" is not enough, and this is the layer where most tools get it wrong.
//! The approach from docs/DESIGN.md §4: for each candidate delimiter, count the fields per
//! line over the first N lines and measure the **variance**. The right delimiter yields zero
//! variance, because a delimited file has the same number of columns on every row. High
//! variance across all candidates means it is not delimited data at all.
//!
//! Quotes are respected while counting, or `"a,b",c` would look like three fields.

/// Candidate delimiters, in preference order for ties.
///
/// Comma first because it is the default; then semicolon (common in locales where the comma
/// is the decimal separator), tab, and finally pipe.
const CANDIDATES: [u8; 4] = [b',', b';', b'\t', b'|'];

/// How many lines to sample. Enough to be confident, few enough to stay cheap on a
/// million-row file.
const SAMPLE_LINES: usize = 20;

/// The minimum consistent lines required before believing it.
///
/// Two lines agreeing is a coincidence; four is a pattern.
const MIN_LINES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delimited {
    pub delimiter: u8,
    pub columns: usize,
    /// How many sampled lines agreed on the column count.
    pub consistent_lines: usize,
}

impl Delimited {
    pub fn delimiter_name(&self) -> &'static str {
        match self.delimiter {
            b',' => "comma",
            b';' => "semicolon",
            b'\t' => "tab",
            b'|' => "pipe",
            _ => "unknown",
        }
    }
}

/// Detects the delimiter of a sample of text.
///
/// Returns `None` when nothing looks consistent enough, which is the common and correct
/// answer for prose.
pub fn detect(text: &str) -> Option<Delimited> {
    // A Markdown table also has a perfectly consistent pipe count per row, so it would be
    // claimed as pipe-delimited data. The GFM separator row is the unambiguous marker that
    // settles it, and Markdown must win: rendering a README's table as a CSV would be
    // absurd.
    if looks_like_markdown_table(text) {
        return None;
    }

    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(SAMPLE_LINES)
        .collect();

    if lines.len() < MIN_LINES.min(2) {
        return None;
    }

    let mut best: Option<Delimited> = None;

    for delimiter in CANDIDATES {
        let counts: Vec<usize> = lines.iter().map(|l| count_fields(l, delimiter)).collect();

        // A single column means the delimiter never appeared.
        let first = counts[0];
        if first < 2 {
            continue;
        }

        // Zero variance is required, not merely low: real delimited data is exact, and
        // accepting "close enough" is what produces the false positives on prose.
        let consistent = counts.iter().all(|c| *c == first);
        if !consistent {
            continue;
        }

        if lines.len() < MIN_LINES {
            // Few lines, so only a delimiter that is unmistakable in prose is accepted. A
            // comma appears in ordinary sentences; a tab or a pipe practically does not.
            if delimiter == b',' || delimiter == b';' {
                continue;
            }
        }

        let candidate = Delimited {
            delimiter,
            columns: first,
            consistent_lines: lines.len(),
        };

        // More columns is stronger evidence: a file consistently showing 7 fields is far more
        // convincing than one showing 2, which prose can hit by chance.
        let better = match &best {
            None => true,
            Some(b) => candidate.columns > b.columns,
        };
        if better {
            best = Some(candidate);
        }
    }

    best
}

/// Counts the fields on a line, honoring double quotes.
fn count_fields(line: &str, delimiter: u8) -> usize {
    let bytes = line.as_bytes();
    let mut fields = 1usize;
    let mut in_quotes = false;
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                // A doubled quote inside a quoted field is an escaped quote, not a
                // delimiter boundary.
                if in_quotes && i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                    i += 1;
                } else {
                    in_quotes = !in_quotes;
                }
            }
            b if b == delimiter && !in_quotes => fields += 1,
            _ => {}
        }
        i += 1;
    }

    fields
}

/// A GFM table separator row: `| --- | :-: |`.
fn looks_like_markdown_table(text: &str) -> bool {
    text.lines().take(SAMPLE_LINES).any(|line| {
        let t = line.trim();
        if !t.contains("---") {
            return false;
        }
        // Only dashes, colons, pipes and spaces: that is a separator row and nothing else.
        t.starts_with('|') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_a_comma_separated_file() {
        let d = detect("a,b,c\n1,2,3\n4,5,6\n7,8,9\n").expect("should detect");
        assert_eq!(d.delimiter, b',');
        assert_eq!(d.columns, 3);
    }

    #[test]
    fn detects_semicolons() {
        let d = detect("name;value\nx;1\ny;2\nz;3\n").expect("should detect");
        assert_eq!(d.delimiter, b';');
        assert_eq!(d.columns, 2);
    }

    #[test]
    fn detects_tabs() {
        let d = detect("a\tb\n1\t2\n3\t4\n5\t6\n").expect("should detect");
        assert_eq!(d.delimiter, b'\t');
        assert_eq!(d.delimiter_name(), "tab");
    }

    #[test]
    fn prose_is_not_delimited_data() {
        // The false positive that matters: ordinary text has commas, but not the same number
        // of them on every line.
        let prose = "This is a sentence, with a comma.\n\
                     This one, however, has two of them.\n\
                     And this one has none\n\
                     A fourth line, for good measure.\n";
        assert!(detect(prose).is_none(), "prose must not look like CSV");
    }

    #[test]
    fn a_markdown_table_is_not_csv() {
        // A Markdown table has a perfectly consistent pipe count, so without the separator
        // check it would be claimed as pipe-delimited data.
        let md = "| Format | Status |\n\
                  | ------ | ------ |\n\
                  | md | ready |\n\
                  | pdf | pending |\n";
        assert!(detect(md).is_none(), "the GFM separator must veto it");
    }

    #[test]
    fn quotes_are_respected() {
        // `"a,b"` is one field, so this file has two columns and not three.
        let d = detect("\"a,b\",c\n\"d,e\",f\n\"g,h\",i\n\"j,k\",l\n").expect("should detect");
        assert_eq!(d.columns, 2, "the quoted comma must not count");
    }

    #[test]
    fn doubled_quotes_do_not_unbalance_the_count() {
        let csv = "\"say \"\"hi\"\"\",b\n\"x\",y\n\"p\",q\n\"r\",s\n";
        let d = detect(csv).expect("should detect");
        assert_eq!(d.columns, 2);
    }

    #[test]
    fn inconsistent_columns_are_rejected() {
        assert!(detect("a,b,c\n1,2\n3,4,5,6\n7,8,9\n").is_none());
    }

    #[test]
    fn more_columns_wins_the_tie() {
        // Both the comma and the pipe are consistent here; the one describing more structure
        // is the real delimiter.
        let text = "a,b|c,d|e\n1,2|3,4|5\n6,7|8,9|0\n1,1|2,2|3\n";
        let d = detect(text).expect("should detect");
        assert_eq!(d.delimiter, b',', "4 comma fields beats 3 pipe fields");
    }

    #[test]
    fn two_lines_of_commas_are_not_enough() {
        // Two lines agreeing is a coincidence. A tab, being almost absent from prose, is
        // accepted with fewer lines.
        assert!(detect("a,b\n1,2\n").is_none());
        assert!(detect("a\tb\n1\t2\n").is_some());
    }

    #[test]
    fn empty_and_tiny_inputs_do_not_panic() {
        assert!(detect("").is_none());
        assert!(detect("\n").is_none());
        assert!(detect("one line only\n").is_none());
    }

    #[test]
    fn blank_lines_do_not_break_consistency() {
        let d = detect("a,b\n\n1,2\n\n3,4\n\n5,6\n").expect("should detect");
        assert_eq!(d.columns, 2);
    }
}
