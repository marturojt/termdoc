//! The engine: turns the event stream into laid-out lines.
//!
//! It is an `Iterator` rather than a function returning a `Vec`, and that is deliberate: a
//! 2 GB log passes through here holding one block in memory, not the document. The only
//! thing that accumulates is the table in progress, because allocating widths requires
//! having seen all of its rows.

use std::borrow::Cow;
use std::collections::VecDeque;

use termdoc_core::{
    BreakKind, Diagnostic, Event, Events, Line, Marker, Metadata, Result, Segment, Severity, Style,
    Tag, TagKind,
};
use termdoc_term::{ColorDepth, Fidelity, UnicodeLevel};

use crate::glyphs::Glyphs;
use crate::table::TableBuilder;
use crate::theme::Theme;
use crate::wrap::{WrapBuffer, display_width, hard_wrap};

/// Width of the line-number gutter, separator included.
const GUTTER_DIGITS: usize = 4;

#[derive(Clone, Debug)]
pub struct LayoutOptions {
    pub width: usize,
    pub fidelity: Fidelity,
    pub theme: Theme,
    /// Numbers the lines of preformatted content and code blocks.
    pub line_numbers: bool,
    /// Emits diagnostics as document lines. The CLI turns this off when the output is a
    /// pipe, so stdout carries only the document and warnings go to stderr.
    pub inline_diagnostics: bool,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        LayoutOptions {
            width: termdoc_term::DEFAULT_WIDTH,
            fidelity: Fidelity::default(),
            theme: Theme::default(),
            line_numbers: false,
            inline_diagnostics: true,
        }
    }
}

/// What a frame contributes to every line's prefix.
#[derive(Clone, Debug, PartialEq)]
enum Indent {
    None,
    Spaces(usize),
    Quote,
}

#[derive(Debug)]
struct Frame {
    kind: TagKind,
    indent: Indent,
}

pub struct Layout<'a> {
    events: Events<'a>,
    opts: LayoutOptions,
    glyphs: &'static Glyphs,

    out: VecDeque<Line<'a>>,
    stack: Vec<Frame>,
    styles: Vec<Style>,
    links: Vec<Cow<'a, str>>,
    buf: WrapBuffer<'a>,
    /// List nesting depth. Numbering comes from the reader in `Marker::Ordered`, so all
    /// that matters here is how deep we are.
    list_depth: usize,
    table: Option<TableBuilder<'a>>,

    /// A bullet waiting to be painted on the first line of a list item.
    pending_marker: Option<Vec<Segment<'a>>>,
    /// Links collected for the reference list, for when the terminal has no OSC 8.
    link_refs: Vec<Cow<'a, str>>,
    diagnostics: Vec<Diagnostic>,
    metadata: Option<Metadata<'a>>,

    gutter_line: usize,
    need_blank: bool,
    emitted: bool,
    finished: bool,
}

impl<'a> Layout<'a> {
    pub fn new(events: Events<'a>, opts: LayoutOptions) -> Self {
        let glyphs = Glyphs::for_level(opts.fidelity.unicode);
        Layout {
            events,
            opts,
            glyphs,
            out: VecDeque::new(),
            stack: Vec::new(),
            styles: Vec::new(),
            links: Vec::new(),
            buf: WrapBuffer::new(),
            list_depth: 0,
            table: None,
            pending_marker: None,
            link_refs: Vec::new(),
            diagnostics: Vec::new(),
            metadata: None,
            gutter_line: 0,
            need_blank: false,
            emitted: false,
            finished: false,
        }
    }

    /// The metadata seen so far. Useful once the stream has been consumed.
    pub fn metadata(&self) -> Option<&Metadata<'a>> {
        self.metadata.as_ref()
    }

    /// Accumulated diagnostics. The CLI dumps these to stderr when it did not emit them
    /// inline.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    // ---------- style and prefix helpers ----------

    fn style(&self) -> Style {
        self.styles.last().copied().unwrap_or(Style::PLAIN)
    }

    fn push_style(&mut self, role: Style) {
        self.styles.push(self.style().over(role));
    }

    fn pop_style(&mut self) {
        self.styles.pop();
    }

    fn link(&self) -> Option<Cow<'a, str>> {
        if self.opts.fidelity.hyperlinks {
            self.links.last().cloned()
        } else {
            None
        }
    }

    /// The prefix for the current lines. With `with_marker`, the pending bullet replaces
    /// the innermost list item's gap.
    fn prefix(&self, with_marker: bool) -> Vec<Segment<'a>> {
        let marker = if with_marker {
            self.pending_marker.as_ref()
        } else {
            None
        };
        let innermost_item = self.stack.iter().rposition(|f| f.kind == TagKind::ListItem);

        let mut out: Vec<Segment<'a>> = Vec::new();
        for (i, frame) in self.stack.iter().enumerate() {
            match &frame.indent {
                Indent::None => {}
                Indent::Spaces(n) => match marker {
                    Some(segs) if Some(i) == innermost_item => {
                        out.extend(segs.iter().cloned());
                    }
                    _ => out.push(Segment::plain(" ".repeat(*n))),
                },
                Indent::Quote => {
                    out.push(Segment::new(
                        self.glyphs.quote_bar.to_string(),
                        self.opts.theme.quote_bar,
                    ));
                    out.push(Segment::plain(" "));
                }
            }
        }
        out
    }

    fn prefix_width(&self, with_marker: bool) -> usize {
        self.prefix(with_marker)
            .iter()
            .map(|s| display_width(&s.text))
            .sum()
    }

    fn in_kind(&self, kind: TagKind) -> bool {
        self.stack.iter().any(|f| f.kind == kind)
    }

    /// The width usable for content, minus the numbering gutter.
    fn content_width(&self) -> usize {
        let gutter = if self.gutter_active() {
            GUTTER_DIGITS + 3
        } else {
            0
        };
        self.opts.width.saturating_sub(gutter)
    }

    fn gutter_active(&self) -> bool {
        self.opts.line_numbers
            && (self.in_kind(TagKind::CodeBlock) || self.in_kind(TagKind::Preformatted))
    }

    // ---------- emission ----------

    /// Pushes a line, trimming trailing spaces.
    ///
    /// A line that ends in spaces dirties copy-paste and diffs, and is the kind of detail
    /// that makes a utility feel foreign to the system.
    fn emit(&mut self, mut line: Line<'a>) {
        loop {
            // The measurements are extracted before mutating so the immutable borrow of
            // the last segment does not stay alive.
            let (full_len, trimmed_len, full_w, trimmed_w) = match line.segments.last() {
                None => break,
                Some(last) => {
                    let t = last.text.trim_end();
                    (
                        last.text.len(),
                        t.len(),
                        display_width(&last.text),
                        display_width(t),
                    )
                }
            };

            if trimmed_len == 0 {
                line.width = line.width.saturating_sub(full_w);
                line.segments.pop();
                continue;
            }

            if trimmed_len != full_len {
                line.width = line.width.saturating_sub(full_w - trimmed_w);
                let last = line
                    .segments
                    .last_mut()
                    .expect("the match guarantees it exists");
                let text = std::mem::replace(&mut last.text, Cow::Borrowed(""));
                last.text = match text {
                    Cow::Borrowed(s) => Cow::Borrowed(&s[..trimmed_len]),
                    Cow::Owned(s) => Cow::Owned(s[..trimmed_len].to_string()),
                };
            }
            break;
        }
        self.emitted = true;
        self.out.push_back(line);
    }

    fn emit_blank(&mut self) {
        self.out.push_back(Line::empty());
        self.emitted = true;
    }

    /// Separates blocks with one blank line, without duplicating them or opening the
    /// document with a gap.
    fn ensure_blank(&mut self) {
        if self.emitted && self.need_blank {
            let prefix = self.prefix(false);
            if prefix.is_empty() {
                self.emit_blank();
            } else {
                let w = self.prefix_width(false);
                let line = Line::from_segments(prefix, w);
                self.emit(line);
            }
            self.need_blank = false;
        }
    }

    /// Flushes the inline buffer as wrapped lines.
    fn flush_inline(&mut self) {
        if self.buf.is_empty() {
            self.pending_marker = None;
            return;
        }
        let width = self.content_width();
        let prefix = self.prefix(false);
        let first = if self.pending_marker.is_some() {
            Some(self.prefix(true))
        } else {
            None
        };
        let lines = self.buf.wrap(width, &prefix, first.as_deref());
        self.buf.clear();
        self.pending_marker = None;
        for line in lines {
            self.emit(line);
        }
    }

    /// One line of preformatted content or code: no reflow, only a hard chop.
    fn emit_pre_line(&mut self, text: Cow<'a, str>, style: Style) {
        let prefix = self.prefix(false);
        let prefix_width: usize = prefix.iter().map(|s| display_width(&s.text)).sum();
        let avail = self.content_width().saturating_sub(prefix_width).max(1);

        // `hard_wrap` borrows slices of the original; a `Cow::Owned` has to be copied.
        let chunks: Vec<Cow<'a, str>> = match &text {
            Cow::Borrowed(s) => hard_wrap(s, avail).into_iter().map(Cow::Borrowed).collect(),
            Cow::Owned(s) => hard_wrap(s, avail)
                .into_iter()
                .map(|c| Cow::Owned(c.to_string()))
                .collect(),
        };

        for (i, chunk) in chunks.into_iter().enumerate() {
            let mut segments = Vec::new();
            if self.gutter_active() {
                if i == 0 {
                    self.gutter_line += 1;
                }
                segments.push(self.gutter_segment(i == 0));
            }
            segments.extend(prefix.iter().cloned());
            segments.push(Segment::new(chunk, style));
            let total: usize = segments.iter().map(|s| display_width(&s.text)).sum();
            self.emit(Line::from_segments(segments, total));
        }
    }

    fn gutter_segment(&self, show: bool) -> Segment<'a> {
        let text = if show {
            format!(
                "{:>w$} {} ",
                self.gutter_line,
                self.glyphs.v,
                w = GUTTER_DIGITS
            )
        } else {
            format!("{:>w$} {} ", "", self.glyphs.v, w = GUTTER_DIGITS)
        };
        Segment::new(text, self.opts.theme.metadata_key)
    }

    /// Emits text the engine generates itself (warnings, labels, references) through the
    /// same wrapping as ordinary content.
    ///
    /// It exists because emitting these lines "by hand" is exactly where the width
    /// invariant escaped: the reference list overflowed in narrow terminals. Everything the
    /// engine generates goes through here.
    fn emit_generated(&mut self, text: String, style: Style, hanging: usize) {
        let mut buf = WrapBuffer::new();
        buf.push_text(Cow::Owned(text), style, None);
        let prefix = self.prefix(false);
        let width = self.content_width();

        let lines = if hanging > 0 {
            let mut cont = prefix.clone();
            cont.push(Segment::plain(" ".repeat(hanging)));
            buf.wrap(width, &cont, Some(&prefix))
        } else {
            buf.wrap(width, &prefix, None)
        };

        for line in lines {
            self.emit(line);
        }
    }

    // ---------- state machine ----------

    fn handle(&mut self, event: Event<'a>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(kind) => self.end(kind),
            Event::Text(text) => self.text(text),
            Event::Code(text) => {
                let style = self.style().over(self.opts.theme.code_inline);
                let link = self.link();
                self.buf.push_text(text, style, link);
            }
            Event::Break(BreakKind::Soft) => {
                self.buf.push_text(Cow::Borrowed(" "), self.style(), None);
            }
            Event::Break(BreakKind::Hard) => self.buf.push_hard_break(),
            Event::Rule => {
                self.ensure_blank();
                self.emit_rule();
                self.need_blank = true;
            }
            Event::PageBreak => {
                // There is no pagination of our own yet (that arrives with the TUI in M2):
                // a rule marks the break without pretending to more than we have.
                self.ensure_blank();
                self.emit_rule();
                self.need_blank = true;
            }
            Event::Image { alt, .. } => {
                // Graphics backends land in M3. Until then, the ladder's lowest rung: the
                // alt text, which is real information.
                let style = self.style().over(self.opts.theme.image_placeholder);
                let text = if alt.is_empty() {
                    Cow::Borrowed("[image]")
                } else {
                    Cow::Owned(format!("[image: {alt}]"))
                };
                self.buf.push_text(text, style, None);
            }
            Event::Math { tex, .. } => {
                let style = self.style().over(self.opts.theme.code_inline);
                self.buf.push_text(tex, style, None);
            }
            Event::FootnoteRef(id) => {
                let style = self.style().over(self.opts.theme.footnote_ref);
                self.buf
                    .push_text(Cow::Owned(format!("[^{id}]")), style, None);
            }
            Event::TaskMarker(done) => {
                let glyph = if done {
                    self.glyphs.task_done
                } else {
                    self.glyphs.task_todo
                };
                let style = self.style().over(self.opts.theme.list_marker);
                self.buf
                    .push_text(Cow::Owned(format!("{glyph} ")), style, None);
            }
            Event::Diagnostic(d) => {
                self.diagnostics.push(d.clone());
                if self.opts.inline_diagnostics {
                    let style = match d.severity {
                        Severity::Error => self.opts.theme.diagnostic_error,
                        _ => self.opts.theme.diagnostic_warning,
                    };
                    let text = format!("{} {}", self.glyphs.diagnostic, d.message);
                    // With a hanging indent: a long warning stays readable instead of
                    // hugging the left margin on its second line.
                    self.emit_generated(text, style, 2);
                }
            }
        }
    }

    fn text(&mut self, text: Cow<'a, str>) {
        // Inside a table, the text belongs to the open cell.
        if let Some(table) = self.table.as_mut() {
            let style = self.styles.last().copied().unwrap_or(Style::PLAIN);
            if let Some(cell) = table.cell_mut() {
                cell.push_text(text, style, None);
            }
            return;
        }

        // In preformatted content and code, the source's line breaks are meaningful.
        if self.in_kind(TagKind::CodeBlock) || self.in_kind(TagKind::Preformatted) {
            let style = self.style();
            match text {
                Cow::Borrowed(s) => {
                    for piece in s.split_inclusive('\n') {
                        let trimmed = piece.trim_end_matches(['\n', '\r']);
                        self.emit_pre_line(Cow::Borrowed(trimmed), style);
                    }
                }
                Cow::Owned(s) => {
                    for piece in s.split_inclusive('\n') {
                        let trimmed = piece.trim_end_matches(['\n', '\r']);
                        self.emit_pre_line(Cow::Owned(trimmed.to_string()), style);
                    }
                }
            }
            return;
        }

        let style = self.style();
        let link = self.link();
        self.buf.push_text(text, style, link);
    }

    fn start(&mut self, tag: Tag<'a>) {
        let kind = tag.kind();

        match tag {
            Tag::Document(meta) => {
                self.metadata = Some(*meta);
                self.stack.push(Frame {
                    kind,
                    indent: Indent::None,
                });
                return;
            }
            Tag::Section { .. } => {
                self.stack.push(Frame {
                    kind,
                    indent: Indent::None,
                });
                return;
            }

            Tag::Heading { level, .. } => {
                self.ensure_blank();
                self.push_style(self.opts.theme.heading_style(level));
                // The heading degradation rung: with color, the style is enough and a
                // prefix would be noise. Without color, a heading and a paragraph are the
                // same text, so the source's own ATX notation is restored. It is
                // information the document already carried.
                if self.opts.fidelity.color == ColorDepth::None {
                    let hashes = "#".repeat(level.clamp(1, 6) as usize);
                    self.buf.push_text(
                        Cow::Owned(format!("{hashes} ")),
                        self.opts.theme.heading_style(level),
                        None,
                    );
                }
            }
            Tag::Paragraph => self.ensure_blank(),
            Tag::Preformatted => {
                self.ensure_blank();
                self.gutter_line = 0;
            }
            Tag::CodeBlock { .. } => {
                self.ensure_blank();
                self.gutter_line = 0;
                self.push_style(self.opts.theme.code_block);
            }
            Tag::BlockQuote { .. } => {
                self.ensure_blank();
                self.push_style(self.opts.theme.quote);
                self.stack.push(Frame {
                    kind,
                    indent: Indent::Quote,
                });
                return;
            }
            Tag::Admonition { kind: akind } => {
                self.ensure_blank();
                let label = match akind {
                    termdoc_core::AdmonitionKind::Note => "NOTE",
                    termdoc_core::AdmonitionKind::Tip => "TIP",
                    termdoc_core::AdmonitionKind::Important => "IMPORTANT",
                    termdoc_core::AdmonitionKind::Warning => "WARNING",
                    termdoc_core::AdmonitionKind::Caution => "CAUTION",
                };
                let style = self.opts.theme.diagnostic_warning;
                let text = format!("{} {label}", self.glyphs.diagnostic);
                self.emit_generated(text, style, 0);
                self.push_style(self.opts.theme.quote);
                self.stack.push(Frame {
                    kind,
                    indent: Indent::Quote,
                });
                return;
            }
            Tag::List { .. } => {
                // A top-level list is separated from the preceding text; a nested one
                // continues inside its item.
                if self.list_depth == 0 {
                    self.ensure_blank();
                } else {
                    self.flush_inline();
                }
                self.list_depth += 1;
                self.stack.push(Frame {
                    kind,
                    indent: Indent::None,
                });
                return;
            }
            Tag::ListItem { marker } => {
                let (text, style) = match marker {
                    Marker::Bullet { depth } => (
                        format!("{} ", self.glyphs.bullet(depth)),
                        self.opts.theme.list_marker,
                    ),
                    Marker::Ordered { number } => {
                        (format!("{number}. "), self.opts.theme.list_marker)
                    }
                };
                let width = display_width(&text);
                self.pending_marker = Some(vec![Segment::new(text, style)]);
                self.stack.push(Frame {
                    kind,
                    indent: Indent::Spaces(width),
                });
                return;
            }
            Tag::Table { align } => {
                self.ensure_blank();
                self.table = Some(TableBuilder::new(align));
                self.stack.push(Frame {
                    kind,
                    indent: Indent::None,
                });
                return;
            }
            Tag::TableHead => {
                if let Some(t) = self.table.as_mut() {
                    t.begin_head();
                    // Markdown emits the header as bare cells, with no row wrapping them.
                    // Without opening one here, those cells land in a row nobody marked as
                    // a header: no bold, no separator.
                    t.begin_row();
                }
                self.stack.push(Frame {
                    kind,
                    indent: Indent::None,
                });
                return;
            }
            Tag::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    t.begin_row();
                }
                self.stack.push(Frame {
                    kind,
                    indent: Indent::None,
                });
                return;
            }
            Tag::TableCell { .. } => {
                if let Some(t) = self.table.as_mut() {
                    t.begin_cell();
                }
                self.stack.push(Frame {
                    kind,
                    indent: Indent::None,
                });
                return;
            }

            // Inline containers: these only push a style.
            Tag::Emphasis => self.push_style(self.opts.theme.emphasis),
            Tag::Strong => self.push_style(self.opts.theme.strong),
            Tag::Strikethrough => self.push_style(self.opts.theme.strikethrough),
            Tag::Underline => self.push_style(self.opts.theme.underline),
            Tag::Highlight => self.push_style(self.opts.theme.highlight),
            Tag::SmallCaps => self.push_style(self.opts.theme.emphasis),
            Tag::Sub | Tag::Super => self.push_style(self.opts.theme.emphasis),
            Tag::Link { href, .. } => {
                self.push_style(self.opts.theme.link);
                self.links.push(href);
            }

            Tag::Figure { .. } | Tag::Footnote { .. } => {
                self.ensure_blank();
            }
            Tag::DefinitionList => self.ensure_blank(),
            Tag::DefinitionTerm => {
                self.ensure_blank();
                self.push_style(self.opts.theme.strong);
            }
            Tag::DefinitionDetail => {
                self.flush_inline();
                self.stack.push(Frame {
                    kind,
                    indent: Indent::Spaces(2),
                });
                return;
            }
        }

        self.stack.push(Frame {
            kind,
            indent: Indent::None,
        });
    }

    fn end(&mut self, kind: TagKind) {
        match kind {
            TagKind::Heading => {
                self.flush_inline();
                self.pop_style();
                self.need_blank = true;
            }
            TagKind::Paragraph => {
                self.flush_inline();
                self.need_blank = true;
            }
            TagKind::Preformatted => {
                self.need_blank = true;
            }
            TagKind::CodeBlock => {
                self.pop_style();
                self.need_blank = true;
            }
            TagKind::BlockQuote | TagKind::Admonition => {
                self.flush_inline();
                self.pop_style();
                self.stack.pop();
                self.need_blank = true;
                return;
            }
            TagKind::List => {
                self.flush_inline();
                self.list_depth = self.list_depth.saturating_sub(1);
                self.stack.pop();
                if self.list_depth == 0 {
                    self.need_blank = true;
                }
                return;
            }
            TagKind::ListItem => {
                self.flush_inline();
                self.stack.pop();
                return;
            }
            TagKind::TableHead => {
                if let Some(t) = self.table.as_mut() {
                    t.end_head();
                }
                self.stack.pop();
                return;
            }
            TagKind::Table => {
                if let Some(table) = self.table.take() {
                    let lines = table.render(
                        self.opts.width,
                        &self.opts.theme,
                        Glyphs::for_level(self.opts.fidelity.unicode),
                    );
                    for line in lines {
                        self.emit(line);
                    }
                }
                self.stack.pop();
                self.need_blank = true;
                return;
            }
            TagKind::Link => {
                self.pop_style();
                let href = self.links.pop();
                // Without OSC 8 the target cannot just be lost: it gets numbered and
                // listed at the end. That is the link degradation rung.
                if !self.opts.fidelity.hyperlinks
                    && let Some(href) = href
                    && !href.is_empty()
                    && !href.starts_with('#')
                {
                    self.link_refs.push(href);
                    let n = self.link_refs.len();
                    let style = self.opts.theme.link_reference;
                    self.buf
                        .push_text(Cow::Owned(format!("[{n}]")), style, None);
                }
                self.stack.pop();
                return;
            }
            TagKind::Emphasis
            | TagKind::Strong
            | TagKind::Strikethrough
            | TagKind::Underline
            | TagKind::Highlight
            | TagKind::SmallCaps
            | TagKind::Sub
            | TagKind::Super
            | TagKind::DefinitionTerm => {
                self.pop_style();
                self.stack.pop();
                return;
            }
            TagKind::DefinitionDetail => {
                self.flush_inline();
                self.stack.pop();
                return;
            }
            TagKind::Document => {
                self.flush_inline();
                self.stack.pop();
                return;
            }
            _ => {
                self.flush_inline();
            }
        }
        self.stack.pop();
    }

    fn emit_rule(&mut self) {
        let prefix = self.prefix(false);
        let prefix_width: usize = prefix.iter().map(|s| display_width(&s.text)).sum();
        let n = self.opts.width.saturating_sub(prefix_width).max(1);
        let mut segments = prefix;
        segments.push(Segment::new(
            self.glyphs.rule.repeat(n),
            self.opts.theme.rule,
        ));
        let total = prefix_width + n * display_width(self.glyphs.rule);
        // A rule is the one case where the "trailing spaces" are content, so it is pushed
        // without going through `emit`'s trimming.
        self.out.push_back(Line::from_segments(segments, total));
        self.emitted = true;
    }

    /// End of document: flush what is pending and append the reference list.
    fn finish(&mut self) {
        self.flush_inline();
        if let Some(table) = self.table.take() {
            let lines = table.render(
                self.opts.width,
                &self.opts.theme,
                Glyphs::for_level(self.opts.fidelity.unicode),
            );
            for line in lines {
                self.emit(line);
            }
        }

        if self.link_refs.is_empty() {
            return;
        }

        self.emit_blank();
        let refs = std::mem::take(&mut self.link_refs);
        let style = self.opts.theme.link_reference;
        for (i, href) in refs.into_iter().enumerate() {
            // A URL longer than the width gets chopped. Some copy-paste convenience is
            // lost, but holding the width invariant is worth more: in a terminal too narrow
            // for the URL, letting it overflow would not have saved it either — the
            // terminal would have broken it anyway, wherever it felt like.
            self.emit_generated(format!("[{}] {href}", i + 1), style, 4);
        }
    }
}

impl<'a> Iterator for Layout<'a> {
    type Item = Result<Line<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(line) = self.out.pop_front() {
                return Some(Ok(line));
            }
            if self.finished {
                return None;
            }
            match self.events.next() {
                None => {
                    self.finish();
                    self.finished = true;
                }
                Some(Err(e)) => {
                    self.finished = true;
                    return Some(Err(e));
                }
                Some(Ok(spanned)) => self.handle(spanned.node),
            }
        }
    }
}

impl std::fmt::Debug for Layout<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Layout")
            .field("width", &self.opts.width)
            .field("stack_depth", &self.stack.len())
            .field("queued", &self.out.len())
            .finish()
    }
}

/// The glyph set for a Unicode level, usable from outside the engine.
pub fn glyphs_for(level: UnicodeLevel) -> &'static Glyphs {
    Glyphs::for_level(level)
}
