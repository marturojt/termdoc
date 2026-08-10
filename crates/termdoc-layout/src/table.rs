//! Tables: column-width allocation and their degradation rung.
//!
//! A table is the one thing in the pipeline that **cannot** be streamed: allocating widths
//! requires having seen every row. The cost is bounded to the table in progress rather than
//! the document, and it is why `WrapBuffer` is re-wrappable: each cell accumulates once and
//! is laid out at whatever width it ends up receiving.
//!
//! Degradation ladder (docs/DESIGN.md §5): Unicode box-drawing → ASCII `+-|` → TSV, once
//! not even the minimum column widths fit.

use termdoc_core::{Align, Line, Segment, Style};

use crate::glyphs::Glyphs;
use crate::theme::Theme;
use crate::wrap::{WrapBuffer, display_width};

/// The minimum column width. Below this the content is illegible and it is better to
/// change representation entirely.
const MIN_COL: usize = 3;

#[derive(Debug, Default)]
pub struct TableBuilder<'a> {
    align: Vec<Align>,
    rows: Vec<Vec<WrapBuffer<'a>>>,
    /// How many leading rows are header rows.
    header_rows: usize,
    in_header: bool,
}

impl<'a> TableBuilder<'a> {
    pub fn new(align: Vec<Align>) -> Self {
        TableBuilder {
            align,
            rows: Vec::new(),
            header_rows: 0,
            in_header: false,
        }
    }

    pub fn begin_head(&mut self) {
        self.in_header = true;
    }

    pub fn end_head(&mut self) {
        self.in_header = false;
    }

    pub fn begin_row(&mut self) {
        self.rows.push(Vec::new());
        if self.in_header {
            self.header_rows = self.rows.len();
        }
    }

    /// Opens a new cell in the current row.
    pub fn begin_cell(&mut self) {
        if self.rows.is_empty() {
            self.rows.push(Vec::new());
        }
        self.rows
            .last_mut()
            .expect("we just guaranteed a row exists")
            .push(WrapBuffer::new());
    }

    /// The open cell, so the engine can accumulate content across several events.
    ///
    /// Deliberately separate from `begin_cell`: a cell receives N text events, and creating
    /// a new cell on each one would scatter them across columns.
    pub fn cell_mut(&mut self) -> Option<&mut WrapBuffer<'a>> {
        self.rows.last_mut().and_then(|r| r.last_mut())
    }

    pub fn is_empty(&self) -> bool {
        self.rows.iter().all(|r| r.is_empty())
    }

    fn columns(&self) -> usize {
        self.rows.iter().map(|r| r.len()).max().unwrap_or(0)
    }

    fn align_for(&self, col: usize) -> Align {
        self.align.get(col).copied().unwrap_or(Align::None)
    }

    /// Lays the table out at the available width.
    pub fn render(&self, width: usize, theme: &Theme, glyphs: &Glyphs) -> Vec<Line<'a>> {
        let ncols = self.columns();
        if ncols == 0 {
            return Vec::new();
        }

        // Each column spends one padding space per side, and there are ncols+1 verticals.
        let chrome = 3 * ncols + 1;
        let content_avail = width.saturating_sub(chrome);

        match self.allocate(ncols, content_avail) {
            Some(widths) => self.render_boxed(&widths, theme, glyphs),
            // Not even the minimums fit: drop to the last rung.
            None => self.render_tsv(),
        }
    }

    /// Distributes `avail` cells across the columns.
    ///
    /// Returns `None` only when the grid does not fit even with every column at `MIN_COL`;
    /// that is the signal to degrade to TSV.
    ///
    /// The shrinking happens in two phases because the two floors are different in kind:
    /// the longest word's width is a *preference* (below it, words get split, which is ugly
    /// but readable), while `MIN_COL` is a hard limit (below it, no content remains).
    /// Treating the preference as a hard limit made a two-column table at 20 cells fall to
    /// TSV for no reason.
    fn allocate(&self, ncols: usize, avail: usize) -> Option<Vec<usize>> {
        if MIN_COL * ncols > avail {
            return None;
        }

        let mut natural = vec![0usize; ncols];
        // Soft floor: below this, words have to be split.
        let mut soft = vec![MIN_COL; ncols];

        for row in &self.rows {
            for (i, cell) in row.iter().enumerate() {
                if i >= ncols {
                    continue;
                }
                natural[i] = natural[i].max(cell.natural_width());
                soft[i] = soft[i].max(cell.longest_word().min(16));
            }
        }

        // Starting point: the natural width, never below the hard minimum.
        let mut widths: Vec<usize> = natural.iter().map(|n| (*n).max(MIN_COL)).collect();
        let mut total: usize = widths.iter().sum();
        if total <= avail {
            return Some(widths);
        }

        // Phase 1: shrink the widest column down to its soft floor.
        while total > avail {
            let victim = widths
                .iter()
                .enumerate()
                .filter(|(i, w)| **w > soft[*i].max(MIN_COL))
                .max_by_key(|(_, w)| **w)
                .map(|(i, _)| i);
            match victim {
                Some(i) => {
                    widths[i] -= 1;
                    total -= 1;
                }
                None => break,
            }
        }

        // Phase 2: still does not fit, so go down to the hard minimum and split words.
        while total > avail {
            let victim = widths
                .iter()
                .enumerate()
                .filter(|(_, w)| **w > MIN_COL)
                .max_by_key(|(_, w)| **w)
                .map(|(i, _)| i)?;
            widths[victim] -= 1;
            total -= 1;
        }

        Some(widths)
    }

    fn render_boxed(&self, widths: &[usize], theme: &Theme, glyphs: &Glyphs) -> Vec<Line<'a>> {
        let mut out = Vec::new();
        let border = theme.table_border;

        out.push(rule_line(
            widths,
            glyphs.top_left,
            glyphs.tee_down,
            glyphs.top_right,
            glyphs.h,
            border,
        ));

        for (idx, row) in self.rows.iter().enumerate() {
            let is_header = idx < self.header_rows;
            let style = if is_header {
                Some(theme.table_header)
            } else {
                None
            };
            out.extend(self.render_row(row, widths, style, theme, glyphs));

            if is_header && idx + 1 == self.header_rows {
                out.push(rule_line(
                    widths,
                    glyphs.tee_right,
                    glyphs.cross,
                    glyphs.tee_left,
                    glyphs.h,
                    border,
                ));
            }
        }

        out.push(rule_line(
            widths,
            glyphs.bottom_left,
            glyphs.tee_up,
            glyphs.bottom_right,
            glyphs.h,
            border,
        ));

        out
    }

    fn render_row(
        &self,
        row: &[WrapBuffer<'a>],
        widths: &[usize],
        header_style: Option<Style>,
        theme: &Theme,
        glyphs: &Glyphs,
    ) -> Vec<Line<'a>> {
        // Each cell wraps to its own width; the physical row takes as many lines as the
        // tallest cell, and the rest are padded blank.
        let wrapped: Vec<Vec<Line<'a>>> = widths
            .iter()
            .enumerate()
            .map(|(i, w)| match row.get(i) {
                Some(cell) => cell.wrap(*w, &[], None),
                None => Vec::new(),
            })
            .collect();

        let height = wrapped.iter().map(|c| c.len()).max().unwrap_or(1).max(1);
        let border = theme.table_border;
        let mut out = Vec::with_capacity(height);

        for line_idx in 0..height {
            let mut segments = vec![Segment::new(glyphs.v.to_string(), border)];
            let mut total = display_width(glyphs.v);

            for (col, w) in widths.iter().enumerate() {
                segments.push(Segment::plain(" "));
                total += 1;

                let content = wrapped[col].get(line_idx);
                let content_width = content.map(|l| l.width).unwrap_or(0);
                let pad = w.saturating_sub(content_width);
                let (left, right) = match self.align_for(col) {
                    Align::Right => (pad, 0),
                    Align::Center => (pad / 2, pad - pad / 2),
                    _ => (0, pad),
                };

                if left > 0 {
                    segments.push(Segment::plain(" ".repeat(left)));
                }
                if let Some(line) = content {
                    for seg in &line.segments {
                        let style = match header_style {
                            Some(hs) => seg.style.over(hs),
                            None => seg.style,
                        };
                        segments.push(Segment {
                            text: seg.text.clone(),
                            style,
                            link: seg.link.clone(),
                        });
                    }
                }
                if right > 0 {
                    segments.push(Segment::plain(" ".repeat(right)));
                }
                total += w;

                segments.push(Segment::plain(" "));
                segments.push(Segment::new(glyphs.v.to_string(), border));
                total += 1 + display_width(glyphs.v);
            }

            out.push(Line::from_segments(segments, total));
        }

        out
    }

    /// The last rung: tab-separated values.
    ///
    /// The grid is lost, but the data is still there and still processable with `cut` or
    /// `awk`, which is more useful than a table squeezed past legibility.
    fn render_tsv(&self) -> Vec<Line<'a>> {
        self.rows
            .iter()
            .map(|row| {
                let mut segments: Vec<Segment<'a>> = Vec::new();
                let mut width = 0usize;
                for (i, cell) in row.iter().enumerate() {
                    if i > 0 {
                        segments.push(Segment::plain("\t"));
                        width += 1;
                    }
                    // Effectively infinite width: nothing wraps here.
                    if let Some(line) = cell.wrap(usize::MAX, &[], None).first() {
                        width += line.width;
                        segments.extend(line.segments.iter().cloned());
                    }
                }
                Line::from_segments(segments, width)
            })
            .collect()
    }
}

fn rule_line<'a>(
    widths: &[usize],
    left: &str,
    mid: &str,
    right: &str,
    horiz: &str,
    style: Style,
) -> Line<'a> {
    let mut s = String::from(left);
    for (i, w) in widths.iter().enumerate() {
        if i > 0 {
            s.push_str(mid);
        }
        // +2 for the cell's padding spaces.
        for _ in 0..(w + 2) {
            s.push_str(horiz);
        }
    }
    s.push_str(right);
    let width = display_width(&s);
    Line::from_segments(vec![Segment::new(s, style)], width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glyphs;
    use std::borrow::Cow;

    fn build(rows: &[&[&str]], header: bool) -> TableBuilder<'static> {
        let mut t = TableBuilder::new(vec![Align::None; rows[0].len()]);
        for (i, row) in rows.iter().enumerate() {
            if header && i == 0 {
                t.begin_head();
            }
            t.begin_row();
            for cell in *row {
                t.begin_cell();
                t.cell_mut()
                    .unwrap()
                    .push_text(Cow::Owned(cell.to_string()), Style::PLAIN, None);
            }
            if header && i == 0 {
                t.end_head();
            }
        }
        t
    }

    fn render(t: &TableBuilder<'_>, width: usize, ascii: bool) -> Vec<String> {
        let g = if ascii {
            &glyphs::ASCII
        } else {
            &glyphs::UNICODE
        };
        t.render(width, &Theme::plain(), g)
            .iter()
            .map(|l| l.to_plain_string())
            .collect()
    }

    #[test]
    fn no_line_exceeds_the_width() {
        let t = build(
            &[
                &["name", "description"],
                &["a", "something fairly long here"],
            ],
            true,
        );
        // 13 is the geometric minimum for two columns: 7 of borders and padding plus
        // 2xMIN_COL of content. Below that it degrades to TSV, which gives up on width
        // deliberately (see `the_tsv_rung_gives_up_on_width_on_purpose`).
        for w in [13usize, 20, 30, 40, 80] {
            for line in render(&t, w, false) {
                assert!(
                    display_width(&line) <= w,
                    "width {w}: '{line}' measures {}",
                    display_width(&line)
                );
            }
        }
    }

    #[test]
    fn two_narrow_columns_still_render_as_a_grid() {
        // Regression: with the longest word's width as a hard floor, this fell to TSV at
        // 20 cells for no reason. Splitting "description" is ugly; losing the grid is
        // worse.
        let t = build(
            &[
                &["name", "description"],
                &["a", "something fairly long here"],
            ],
            true,
        );
        let lines = render(&t, 20, false);
        assert!(
            lines.iter().any(|l| l.contains('│')),
            "expected a grid, not TSV: {lines:?}"
        );
    }

    #[test]
    fn the_tsv_rung_gives_up_on_width_on_purpose() {
        // The last rung: the data is preserved even though it overflows. Truncating would
        // lose information, and by this point the output is meant for `cut`/`awk` rather
        // than for reading on screen.
        let t = build(&[&["aaa", "bbb", "ccc", "ddd", "eee", "fff"]], false);
        let lines = render(&t, 10, false);
        assert!(lines[0].contains('\t'));
        assert!(
            display_width(&lines[0]) > 10,
            "this is the case we are documenting"
        );
    }

    #[test]
    fn the_grid_is_square() {
        // Every line of a table must measure exactly the same, or the vertical borders do
        // not line up.
        let t = build(&[&["a", "b"], &["ccc", "d"]], true);
        let lines = render(&t, 40, false);
        let first = display_width(&lines[0]);
        for l in &lines {
            assert_eq!(display_width(l), first, "'{l}' breaks the grid alignment");
        }
    }

    #[test]
    fn degrades_to_ascii_without_box_drawing() {
        let t = build(&[&["a", "b"], &["c", "d"]], true);
        for line in render(&t, 40, true) {
            assert!(line.is_ascii(), "'{line}' should be pure ASCII");
        }
    }

    #[test]
    fn degrades_to_tsv_when_it_cannot_fit() {
        // Six columns in ten cells: no legible grid is possible.
        let t = build(&[&["aaa", "bbb", "ccc", "ddd", "eee", "fff"]], false);
        let lines = render(&t, 10, false);
        assert_eq!(lines.len(), 1, "TSV is one line per row, with no borders");
        assert!(lines[0].contains('\t'), "it should be tab-separated");
        assert!(!lines[0].contains('│'), "no grid should remain");
    }

    #[test]
    fn long_cells_wrap_across_several_lines() {
        let t = build(&[&["k", "a long phrase that will not fit at once"]], false);
        let lines = render(&t, 30, false);
        // Top border, several content lines, bottom border.
        assert!(lines.len() > 3, "expected several lines: {lines:?}");
    }

    #[test]
    fn an_empty_table_produces_nothing() {
        let t = TableBuilder::new(vec![]);
        assert!(t.render(80, &Theme::plain(), &glyphs::UNICODE).is_empty());
    }

    #[test]
    fn right_alignment() {
        let mut t = TableBuilder::new(vec![Align::Right]);
        t.begin_row();
        t.begin_cell();
        t.cell_mut()
            .unwrap()
            .push_text(Cow::Borrowed("7"), Style::PLAIN, None);
        t.begin_row();
        t.begin_cell();
        t.cell_mut()
            .unwrap()
            .push_text(Cow::Borrowed("1234"), Style::PLAIN, None);
        let lines = render(&t, 20, true);
        // The '7' cell must sit flush against the right edge of its column.
        let row = lines.iter().find(|l| l.contains('7')).expect("row with 7");
        assert!(row.contains("   7 |") || row.contains("7 |"), "'{row}'");
    }
}
