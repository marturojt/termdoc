//! Unicode-aware wrapping over styled text.
//!
//! The invariants pinned down by the property tests in `tests/invariants.rs`, **in order
//! of precedence**:
//!
//! 1. A break never splits a grapheme cluster.
//! 2. No line exceeds the requested width **in display cells** (not in bytes, and not in
//!    `char`s: an ideograph takes two columns and a combining accent takes none).
//! 3. A word wider than the line is chopped rather than allowed to overflow.
//! 4. No visible text is lost or invented: this is a viewer, not an editor.
//!
//! ## Why 1 outranks 2
//!
//! The first two collide in a real case: a ZWJ emoji is two cells wide and a single
//! cluster, so at a width of one column satisfying both is impossible. Cluster integrity
//! wins.
//!
//! The reason is the asymmetry of the damage. Splitting a cluster produces visible garbage
//! —half an emoji, a stray accent floating on its own— and does not even fix anything,
//! because the fragments still occupy cells. Overflowing by one column in a one-column
//! terminal is a degenerate case where nothing was going to read well anyway. Therefore:
//! **the width is firm for everything except an indivisible cluster that does not fit on
//! its own.**

use std::borrow::Cow;

use termdoc_core::{Line, Segment, Style};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// How wide a tab expands to. M0 expands it to a fixed number of spaces; real tab stops
/// depend on the column and can wait until some format genuinely needs them.
pub const TAB_WIDTH: usize = 4;

/// The minimum usable width. Below this, wrapping cannot make progress and would loop, so
/// this floor is enforced.
const MIN_WIDTH: usize = 1;

pub fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Word,
    Space,
    HardBreak,
}

#[derive(Clone, Debug)]
struct Piece<'a> {
    text: Cow<'a, str>,
    width: usize,
    style: Style,
    link: Option<Cow<'a, str>>,
    kind: Kind,
}

/// Accumulates styled text and distributes it into lines when closed.
///
/// It fills up per paragraph, not per document: the memory peak is one paragraph, not the
/// file. A 2 GB log passes through here one line at a time.
#[derive(Debug, Default)]
pub struct WrapBuffer<'a> {
    pieces: Vec<Piece<'a>>,
}

impl<'a> WrapBuffer<'a> {
    pub fn new() -> Self {
        WrapBuffer { pieces: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// `true` when there is nothing but spaces: used to avoid emitting empty paragraphs.
    pub fn is_blank(&self) -> bool {
        self.pieces.iter().all(|p| p.kind == Kind::Space)
    }

    pub fn clear(&mut self) {
        self.pieces.clear();
    }

    /// The width all the content would take on a single line. This is a table column's
    /// "ideal" width before any allocation.
    pub fn natural_width(&self) -> usize {
        let mut best = 0usize;
        let mut cur = 0usize;
        for p in &self.pieces {
            if p.kind == Kind::HardBreak {
                best = best.max(cur);
                cur = 0;
            } else {
                cur += p.width;
            }
        }
        best.max(cur)
    }

    /// The width of the longest word: below this, a table column would be forced to split
    /// words, which is what we would rather avoid.
    pub fn longest_word(&self) -> usize {
        self.pieces
            .iter()
            .filter(|p| p.kind == Kind::Word)
            .map(|p| p.width)
            .max()
            .unwrap_or(0)
    }

    pub fn push_hard_break(&mut self) {
        self.pieces.push(Piece {
            text: Cow::Borrowed(""),
            width: 0,
            style: Style::PLAIN,
            link: None,
            kind: Kind::HardBreak,
        });
    }

    /// Appends text, chopping it into words and runs of spaces.
    ///
    /// It borrows from the original whenever the incoming `Cow` is borrowed: this is the
    /// path by which text from an `mmap` reaches the output without a single copy.
    pub fn push_text(&mut self, text: Cow<'a, str>, style: Style, link: Option<Cow<'a, str>>) {
        if text.is_empty() {
            return;
        }

        // A tab has width 0 as far as `unicode-width` is concerned, so expanding it here
        // keeps the column arithmetic from lying later on.
        if text.contains('\t') {
            let expanded = text.replace('\t', &" ".repeat(TAB_WIDTH));
            self.push_runs(Cow::Owned(expanded), style, link);
        } else {
            self.push_runs(text, style, link);
        }
    }

    fn push_runs(&mut self, text: Cow<'a, str>, style: Style, link: Option<Cow<'a, str>>) {
        // The text is split into homogeneous runs (all space / all non-space) by walking
        // grapheme clusters rather than `char`s, so "e" plus a combining accent is never
        // torn apart.
        let mut runs: Vec<(usize, usize, bool)> = Vec::new();
        let mut run_start = 0usize;
        let mut run_is_space: Option<bool> = None;

        for (idx, gr) in text.grapheme_indices(true) {
            let is_space = !gr.is_empty() && gr.chars().all(char::is_whitespace);
            match run_is_space {
                None => {
                    run_is_space = Some(is_space);
                    run_start = idx;
                }
                Some(prev) if prev == is_space => {}
                Some(prev) => {
                    runs.push((run_start, idx, prev));
                    run_start = idx;
                    run_is_space = Some(is_space);
                }
            }
        }
        if let Some(prev) = run_is_space {
            runs.push((run_start, text.len(), prev));
        }

        for (start, end, is_space) in runs {
            let slice: Cow<'a, str> = match &text {
                // Borrowed: the slice still points into the original source.
                Cow::Borrowed(s) => Cow::Borrowed(&s[start..end]),
                // Owned: the slice has to be copied, there is no alternative.
                Cow::Owned(s) => Cow::Owned(s[start..end].to_string()),
            };
            let width = display_width(&slice);
            self.pieces.push(Piece {
                text: slice,
                width,
                style,
                link: link.clone(),
                kind: if is_space { Kind::Space } else { Kind::Word },
            });
        }
    }

    /// Distributes the accumulated content into lines of at most `width` cells.
    ///
    /// `prefix` is prepended to every line and its width **counts** against `width`;
    /// `first_prefix`, when given, replaces it on the first line only. That is what lets a
    /// list bullet hang while the text stays aligned.
    // The final `flush!` leaves assignments nobody reads. Keeping the macro uniform is
    // worth more than saving those two writes.
    #[allow(unused_assignments)]
    pub fn wrap(
        &self,
        width: usize,
        prefix: &[Segment<'a>],
        first_prefix: Option<&[Segment<'a>]>,
    ) -> Vec<Line<'a>> {
        let prefix_width = segments_width(prefix);
        let first_prefix_width = first_prefix.map(segments_width).unwrap_or(prefix_width);

        let mut lines: Vec<Line<'a>> = Vec::new();
        let mut cur: Vec<Piece<'a>> = Vec::new();
        let mut cur_width = 0usize;
        // Spaces are held pending so trailing ones never get emitted: a line with leftover
        // spaces dirties copy-paste and diffs.
        let mut pending_space: Option<Piece<'a>> = None;

        let avail_for = |line_no: usize| -> usize {
            let used = if line_no == 0 {
                first_prefix_width
            } else {
                prefix_width
            };
            width.saturating_sub(used).max(MIN_WIDTH)
        };

        macro_rules! flush {
            () => {{
                let pfx: &[Segment<'a>] = if lines.is_empty() {
                    first_prefix.unwrap_or(prefix)
                } else {
                    prefix
                };
                lines.push(build_line(pfx, &cur));
                cur.clear();
                cur_width = 0;
                pending_space = None;
            }};
        }

        for piece in &self.pieces {
            match piece.kind {
                Kind::HardBreak => flush!(),
                Kind::Space => {
                    // A space at the start of a line is dropped.
                    if !cur.is_empty() {
                        pending_space = Some(piece.clone());
                    }
                }
                Kind::Word => {
                    let avail = avail_for(lines.len());
                    let space_w = pending_space.as_ref().map(|p| p.width).unwrap_or(0);

                    if !cur.is_empty() && cur_width + space_w + piece.width > avail {
                        // Does not fit: break here and drop the pending space.
                        flush!();
                    }

                    let avail = avail_for(lines.len());

                    if piece.width > avail {
                        // A word wider than the whole line (a long URL, a 1 MB line with
                        // no spaces): chop it at grapheme boundaries.
                        if let Some(sp) = pending_space.take()
                            && !cur.is_empty()
                        {
                            cur_width += sp.width;
                            cur.push(sp);
                        }
                        for chunk in split_to_width(piece, avail.saturating_sub(cur_width).max(1)) {
                            if cur_width + chunk.width > avail_for(lines.len()) && !cur.is_empty() {
                                flush!();
                            }
                            cur_width += chunk.width;
                            cur.push(chunk);
                            if cur_width >= avail_for(lines.len()) {
                                flush!();
                            }
                        }
                    } else {
                        if let Some(sp) = pending_space.take()
                            && !cur.is_empty()
                        {
                            cur_width += sp.width;
                            cur.push(sp);
                        }
                        cur_width += piece.width;
                        cur.push(piece.clone());
                    }
                }
            }
        }

        if !cur.is_empty() {
            flush!();
        }

        // A buffer that held only spaces yields a line with the prefix, not nothing: in an
        // empty block quote the `│` must still show up.
        if lines.is_empty() && !self.pieces.is_empty() {
            lines.push(build_line(first_prefix.unwrap_or(prefix), &[]));
        }

        lines
    }
}

fn segments_width(segments: &[Segment<'_>]) -> usize {
    segments.iter().map(|s| display_width(&s.text)).sum()
}

/// Chops a piece into fragments no wider than `first_avail` cells.
fn split_to_width<'a>(piece: &Piece<'a>, first_avail: usize) -> Vec<Piece<'a>> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut acc = 0usize;
    let limit = first_avail.max(MIN_WIDTH);

    for (idx, gr) in piece.text.grapheme_indices(true) {
        let w = display_width(gr);
        if acc + w > limit && idx > start {
            out.push(sub_piece(piece, start, idx, acc));
            start = idx;
            acc = 0;
        }
        acc += w;
    }
    if start < piece.text.len() {
        out.push(sub_piece(piece, start, piece.text.len(), acc));
    }
    out
}

fn sub_piece<'a>(piece: &Piece<'a>, start: usize, end: usize, width: usize) -> Piece<'a> {
    let text: Cow<'a, str> = match &piece.text {
        Cow::Borrowed(s) => Cow::Borrowed(&s[start..end]),
        Cow::Owned(s) => Cow::Owned(s[start..end].to_string()),
    };
    Piece {
        text,
        width,
        style: piece.style,
        link: piece.link.clone(),
        kind: Kind::Word,
    }
}

/// Builds the line by borrowing each piece as-is.
///
/// It deliberately does **not** merge adjacent pieces of the same style: doing so would
/// require concatenating strings and we would lose the `Cow::Borrowed` that comes from the
/// `mmap`, which is exactly what keeps memory usage flat. Avoiding redundant ANSI
/// sequences is the backend's job — it already tracks the current style as state and
/// solves it without copying anything.
fn build_line<'a>(prefix: &[Segment<'a>], pieces: &[Piece<'a>]) -> Line<'a> {
    let mut segments: Vec<Segment<'a>> = prefix.to_vec();
    let mut width = segments_width(prefix);

    for piece in pieces {
        width += piece.width;
        segments.push(Segment {
            text: piece.text.clone(),
            style: piece.style,
            link: piece.link.clone(),
        });
    }

    Line::from_segments(segments, width)
}

/// Chops a string into lines of at most `width` cells without reflowing words.
///
/// Preformatted content and code blocks use this, where the source's line breaks are
/// meaningful and all that is needed is to avoid overflow.
pub fn hard_wrap(text: &str, width: usize) -> Vec<&str> {
    let width = width.max(MIN_WIDTH);
    if display_width(text) <= width {
        return vec![text];
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut acc = 0usize;
    for (idx, gr) in text.grapheme_indices(true) {
        let w = display_width(gr);
        if acc + w > width && idx > start {
            out.push(&text[start..idx]);
            start = idx;
            acc = 0;
        }
        acc += w;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    if out.is_empty() {
        out.push("");
    }
    out
}

/// One run of text in one style: a line of preformatted content is a list of these.
pub type Part<'a> = (Cow<'a, str>, Style);

/// [`hard_wrap`] for a line made of several styled runs.
///
/// Chops at `width` cells, never inside a grapheme cluster, and keeps every run's style on
/// the pieces it is split into. Borrowed runs stay borrowed.
pub fn hard_wrap_parts<'a>(parts: Vec<Part<'a>>, width: usize) -> Vec<Vec<Part<'a>>> {
    let width = width.max(MIN_WIDTH);
    let total: usize = parts.iter().map(|(t, _)| display_width(t)).sum();
    if total <= width {
        return vec![parts];
    }

    fn slice<'a>(text: &Cow<'a, str>, from: usize, to: usize) -> Cow<'a, str> {
        match text {
            Cow::Borrowed(s) => Cow::Borrowed(&s[from..to]),
            Cow::Owned(s) => Cow::Owned(s[from..to].to_string()),
        }
    }

    let mut out: Vec<Vec<Part<'a>>> = Vec::new();
    let mut current: Vec<Part<'a>> = Vec::new();
    let mut acc = 0usize;
    for (text, style) in parts {
        let mut start = 0usize;
        for (idx, gr) in text.grapheme_indices(true) {
            let w = display_width(gr);
            if acc + w > width && acc > 0 {
                if idx > start {
                    current.push((slice(&text, start, idx), style));
                }
                out.push(std::mem::take(&mut current));
                start = idx;
                acc = 0;
            }
            acc += w;
        }
        if start < text.len() {
            current.push((slice(&text, start, text.len()), style));
        }
    }
    if !current.is_empty() || out.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap_plain(text: &str, width: usize) -> Vec<String> {
        let mut b = WrapBuffer::new();
        b.push_text(Cow::Borrowed(text), Style::PLAIN, None);
        b.wrap(width, &[], None)
            .iter()
            .map(|l| l.to_plain_string())
            .collect()
    }

    #[test]
    fn breaks_at_word_boundaries() {
        assert_eq!(
            wrap_plain("the quick brown fox", 10),
            vec!["the quick", "brown fox"]
        );
    }

    #[test]
    fn leaves_no_trailing_spaces() {
        for line in wrap_plain("one two three four five six", 10) {
            assert_eq!(line, line.trim_end(), "'{line}' ends with a space");
        }
    }

    #[test]
    fn chops_words_wider_than_the_line() {
        let lines = wrap_plain("aaaaaaaaaaaaaaaaaaaa", 6);
        assert!(lines.len() > 1);
        for l in &lines {
            assert!(display_width(l) <= 6, "'{l}' overflows");
        }
        assert_eq!(
            lines.concat(),
            "aaaaaaaaaaaaaaaaaaaa",
            "without losing text"
        );
    }

    #[test]
    fn counts_cells_not_characters_for_cjk() {
        // Each ideograph takes two columns: four of them fill a width of 8.
        let lines = wrap_plain("日本語日本語日本語", 8);
        for l in &lines {
            assert!(display_width(l) <= 8, "'{l}' = {} cells", display_width(l));
        }
    }

    #[test]
    fn does_not_split_grapheme_clusters() {
        // A ZWJ family: one cluster that must not end up chopped.
        let text = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        let lines = wrap_plain(text, 2);
        for l in &lines {
            assert!(
                !l.starts_with('\u{200d}') && !l.ends_with('\u{200d}'),
                "cluster split in '{l}'"
            );
        }
    }

    #[test]
    fn honors_hard_breaks() {
        let mut b = WrapBuffer::new();
        b.push_text(Cow::Borrowed("one"), Style::PLAIN, None);
        b.push_hard_break();
        b.push_text(Cow::Borrowed("two"), Style::PLAIN, None);
        let lines: Vec<_> = b
            .wrap(80, &[], None)
            .iter()
            .map(|l| l.to_plain_string())
            .collect();
        assert_eq!(lines, vec!["one", "two"]);
    }

    #[test]
    fn the_prefix_counts_against_the_width() {
        let mut b = WrapBuffer::new();
        b.push_text(Cow::Borrowed("one two three"), Style::PLAIN, None);
        let prefix = vec![Segment::plain("| ")];
        let lines = b.wrap(8, &prefix, None);
        for l in &lines {
            assert!(l.width <= 8, "'{}' = {}", l.to_plain_string(), l.width);
            assert!(l.to_plain_string().starts_with("| "));
        }
    }

    #[test]
    fn first_prefix_allows_a_hanging_bullet() {
        let mut b = WrapBuffer::new();
        b.push_text(Cow::Borrowed("one two three four"), Style::PLAIN, None);
        let cont = vec![Segment::plain("  ")];
        let first = vec![Segment::plain("- ")];
        let lines: Vec<_> = b
            .wrap(12, &cont, Some(&first))
            .iter()
            .map(|l| l.to_plain_string())
            .collect();
        assert!(lines[0].starts_with("- "));
        for l in &lines[1..] {
            assert!(l.starts_with("  "), "unindented continuation: '{l}'");
        }
    }

    #[test]
    fn expands_tabs() {
        let lines = wrap_plain("a\tb", 80);
        assert_eq!(lines[0], format!("a{}b", " ".repeat(TAB_WIDTH)));
    }

    #[test]
    fn zero_width_does_not_loop_forever() {
        // Before the MIN_WIDTH floor, this hung the process.
        let lines = wrap_plain("abc def", 0);
        assert!(!lines.is_empty());
    }

    #[test]
    fn source_text_is_never_copied() {
        // The memory invariant: what comes in borrowed goes out borrowed. If this starts
        // failing, a 2 GB log begins costing 2 GB of heap.
        let mut b = WrapBuffer::new();
        b.push_text(Cow::Borrowed("one two three"), Style::PLAIN, None);
        let lines = b.wrap(80, &[], None);
        for seg in &lines[0].segments {
            assert!(
                matches!(seg.text, Cow::Borrowed(_)),
                "'{}' was copied to the heap",
                seg.text
            );
        }
    }

    #[test]
    fn hard_wrap_parts_keeps_each_runs_style_across_a_chop() {
        let a = Style::bold();
        let parts = vec![
            (Cow::Borrowed("abcd"), a),
            (Cow::Borrowed("efgh"), Style::PLAIN),
        ];
        let lines = hard_wrap_parts(parts, 6);
        let shown: Vec<Vec<(String, bool)>> = lines
            .iter()
            .map(|l| l.iter().map(|(t, s)| (t.to_string(), *s == a)).collect())
            .collect();
        assert_eq!(
            shown,
            vec![
                vec![("abcd".into(), true), ("ef".into(), false)],
                vec![("gh".into(), false)],
            ]
        );
    }

    #[test]
    fn hard_wrap_parts_stays_borrowed() {
        let lines = hard_wrap_parts(vec![(Cow::Borrowed("abcdef"), Style::PLAIN)], 3);
        assert!(
            lines
                .iter()
                .flatten()
                .all(|(t, _)| matches!(t, Cow::Borrowed(_)))
        );
    }

    #[test]
    fn hard_wrap_does_not_reflow() {
        assert_eq!(hard_wrap("abcdef", 3), vec!["abc", "def"]);
        assert_eq!(hard_wrap("ab", 10), vec!["ab"]);
    }
}
