//! The Markdown reader, built on `pulldown-cmark`.
//!
//! The mapping is nearly mechanical, and that is no accident: `pulldown-cmark` also models
//! the document as a stream of `Start`/`End` events, which is exactly the model adopted in
//! `termdoc-core`. Translating between two streams is a `match`; translating a stream into a
//! tree would be a parser.
//!
//! What this reader **does** add on top of the direct mapping:
//!
//! - It numbers list items (`pulldown` does not) and tracks depth so bullets rotate.
//! - It accumulates image alt text, which `pulldown` delivers as loose events between
//!   `Start(Image)` and `End(Image)`.
//! - It translates GFM alerts (`> [!WARNING]`) into `Tag::Admonition`.

use std::borrow::Cow;
use std::collections::VecDeque;

use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, CowStr, Event as MdEvent, HeadingLevel, Options,
    Parser, Tag as MdTag, TagEnd as MdTagEnd,
};
use termdoc_core::{
    AdmonitionKind, Align, BreakKind, DocumentReader, Event, Events, FormatId, Marker, Metadata,
    ReadContext, ReaderCaps, Result, Source, Span, Spanned, Tag, TagKind,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct MarkdownReader;

impl MarkdownReader {
    pub fn new() -> Self {
        MarkdownReader
    }

    fn options() -> Options {
        // Everything a modern README is expected to use. `ENABLE_SMART_PUNCTUATION` is
        // deliberately left off: turning `--` into an em dash and quotes into typographic
        // ones alters the document's text, and in a viewer that is an unpleasant surprise
        // when what you are reading is code or a path.
        Options::ENABLE_TABLES
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_HEADING_ATTRIBUTES
            | Options::ENABLE_MATH
            | Options::ENABLE_GFM
    }
}

impl DocumentReader for MarkdownReader {
    fn id(&self) -> FormatId {
        FormatId::Markdown
    }

    fn capabilities(&self) -> ReaderCaps {
        ReaderCaps {
            streaming: true,
            paginated: false,
            metadata: false,
        }
    }

    fn read<'a>(&self, src: &'a Source, _ctx: &ReadContext) -> Result<Events<'a>> {
        let (text, _lossy) = src.as_str();
        let parser = Parser::new_ext(text, Self::options()).into_offset_iter();

        let mut queue = VecDeque::new();
        let meta = Metadata {
            source_format: Some(FormatId::Markdown),
            ..Metadata::default()
        };
        queue.push_back(Spanned::bare(Event::Start(Tag::Document(Box::new(meta)))));

        Ok(Box::new(MarkdownEvents {
            parser,
            queue,
            lists: Vec::new(),
            image_alt: None,
            pending_image: None,
            closed: false,
        }))
    }
}

#[derive(Debug)]
struct ListCtx {
    ordered: bool,
    next: u64,
    depth: u8,
}

struct MarkdownEvents<'a> {
    parser: pulldown_cmark::OffsetIter<'a>,
    queue: VecDeque<Spanned<Event<'a>>>,
    lists: Vec<ListCtx>,
    /// Alt text for the image in progress. While this is `Some`, text accumulates here
    /// instead of being emitted.
    image_alt: Option<String>,
    /// That image's destination, held until the alt text is complete.
    pending_image: Option<Cow<'a, str>>,
    closed: bool,
}

impl<'a> Iterator for MarkdownEvents<'a> {
    type Item = Result<Spanned<Event<'a>>>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(ev) = self.queue.pop_front() {
                return Some(Ok(ev));
            }
            match self.parser.next() {
                Some((event, range)) => {
                    let span = Span::bytes(range.start as u64, range.end as u64);
                    self.translate(event, span);
                }
                None => {
                    if self.closed {
                        return None;
                    }
                    self.closed = true;
                    self.queue
                        .push_back(Spanned::bare(Event::End(TagKind::Document)));
                }
            }
        }
    }
}

impl<'a> MarkdownEvents<'a> {
    fn emit(&mut self, event: Event<'a>, span: Span) {
        self.queue.push_back(Spanned::new(event, span));
    }

    fn translate(&mut self, event: MdEvent<'a>, span: Span) {
        match event {
            MdEvent::Start(tag) => self.start(tag, span),
            MdEvent::End(end) => self.end(end, span),

            MdEvent::Text(text) => {
                // Inside an image, text is its alternative, not content.
                if let Some(alt) = self.image_alt.as_mut() {
                    alt.push_str(&text);
                } else {
                    self.emit(Event::Text(cow(text)), span);
                }
            }
            MdEvent::Code(text) => self.emit(Event::Code(cow(text)), span),
            MdEvent::SoftBreak => self.emit(Event::Break(BreakKind::Soft), span),
            MdEvent::HardBreak => self.emit(Event::Break(BreakKind::Hard), span),
            MdEvent::Rule => self.emit(Event::Rule, span),
            MdEvent::TaskListMarker(done) => self.emit(Event::TaskMarker(done), span),
            MdEvent::FootnoteReference(id) => self.emit(Event::FootnoteRef(cow(id)), span),
            MdEvent::InlineMath(tex) => self.emit(
                Event::Math {
                    inline: true,
                    tex: cow(tex),
                },
                span,
            ),
            MdEvent::DisplayMath(tex) => self.emit(
                Event::Math {
                    inline: false,
                    tex: cow(tex),
                },
                span,
            ),

            // Raw HTML inside Markdown. Dropped silently: interpreting it here would be a
            // bad reimplementation of the HTML reader, and dumping the tags as text makes
            // the output harder to read. The M3 HTML reader is the one that knows how to do
            // this properly.
            MdEvent::Html(_) | MdEvent::InlineHtml(_) => {}
        }
    }

    fn start(&mut self, tag: MdTag<'a>, span: Span) {
        match tag {
            MdTag::Paragraph => self.emit(Event::Start(Tag::Paragraph), span),

            MdTag::Heading { level, id, .. } => self.emit(
                Event::Start(Tag::Heading {
                    level: heading_level(level),
                    id: id.map(cow),
                }),
                span,
            ),

            MdTag::CodeBlock(kind) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => {
                        // The info string can carry more than the language (```rust,ignore):
                        // the language is the first token.
                        let first = info.split([',', ' ']).next().unwrap_or("").to_string();
                        if first.is_empty() {
                            None
                        } else {
                            Some(Cow::Owned(first))
                        }
                    }
                    CodeBlockKind::Indented => None,
                };
                self.emit(
                    Event::Start(Tag::CodeBlock {
                        lang,
                        filename: None,
                    }),
                    span,
                );
            }

            MdTag::BlockQuote(kind) => match kind {
                // GFM alerts: `> [!WARNING]` is semantically an admonition, not a quote, and
                // deserves to be painted as one.
                Some(k) => self.emit(
                    Event::Start(Tag::Admonition {
                        kind: admonition(k),
                    }),
                    span,
                ),
                None => self.emit(Event::Start(Tag::BlockQuote { attribution: None }), span),
            },

            MdTag::List(start) => {
                let ordered = start.is_some();
                let depth = self.lists.len() as u8;
                self.lists.push(ListCtx {
                    ordered,
                    next: start.unwrap_or(1),
                    depth,
                });
                self.emit(
                    Event::Start(Tag::List {
                        ordered,
                        start: start.unwrap_or(1),
                        tight: true,
                    }),
                    span,
                );
            }

            MdTag::Item => {
                // `pulldown` does not number items: the reader keeps the count, since it is
                // the one that knows the nesting.
                let marker = match self.lists.last_mut() {
                    Some(ctx) if ctx.ordered => {
                        let n = ctx.next;
                        ctx.next += 1;
                        Marker::Ordered { number: n }
                    }
                    Some(ctx) => Marker::Bullet { depth: ctx.depth },
                    None => Marker::Bullet { depth: 0 },
                };
                self.emit(Event::Start(Tag::ListItem { marker }), span);
            }

            MdTag::Table(alignments) => self.emit(
                Event::Start(Tag::Table {
                    align: alignments.into_iter().map(align).collect(),
                }),
                span,
            ),
            MdTag::TableHead => self.emit(Event::Start(Tag::TableHead), span),
            MdTag::TableRow => self.emit(Event::Start(Tag::TableRow), span),
            MdTag::TableCell => self.emit(
                Event::Start(Tag::TableCell {
                    colspan: 1,
                    rowspan: 1,
                }),
                span,
            ),

            MdTag::Emphasis => self.emit(Event::Start(Tag::Emphasis), span),
            MdTag::Strong => self.emit(Event::Start(Tag::Strong), span),
            MdTag::Strikethrough => self.emit(Event::Start(Tag::Strikethrough), span),
            MdTag::Superscript => self.emit(Event::Start(Tag::Super), span),
            MdTag::Subscript => self.emit(Event::Start(Tag::Sub), span),

            MdTag::Link {
                dest_url, title, ..
            } => self.emit(
                Event::Start(Tag::Link {
                    href: cow(dest_url),
                    title: if title.is_empty() {
                        None
                    } else {
                        Some(cow(title))
                    },
                }),
                span,
            ),

            MdTag::Image { dest_url, .. } => {
                // Nothing is emitted yet: the alt text has to be gathered first.
                self.image_alt = Some(String::new());
                self.pending_image = Some(cow(dest_url));
            }

            MdTag::FootnoteDefinition(id) => {
                self.emit(Event::Start(Tag::Footnote { id: cow(id) }), span)
            }
            MdTag::DefinitionList => self.emit(Event::Start(Tag::DefinitionList), span),
            MdTag::DefinitionListTitle => self.emit(Event::Start(Tag::DefinitionTerm), span),
            MdTag::DefinitionListDefinition => self.emit(Event::Start(Tag::DefinitionDetail), span),

            // Blocks this viewer does not represent structurally.
            MdTag::HtmlBlock | MdTag::MetadataBlock(_) => {}
        }
    }

    fn end(&mut self, end: MdTagEnd, span: Span) {
        let kind = match end {
            MdTagEnd::Paragraph => TagKind::Paragraph,
            MdTagEnd::Heading(_) => TagKind::Heading,
            MdTagEnd::CodeBlock => TagKind::CodeBlock,
            MdTagEnd::BlockQuote(Some(_)) => TagKind::Admonition,
            MdTagEnd::BlockQuote(None) => TagKind::BlockQuote,
            MdTagEnd::List(_) => {
                self.lists.pop();
                TagKind::List
            }
            MdTagEnd::Item => TagKind::ListItem,
            MdTagEnd::Table => TagKind::Table,
            MdTagEnd::TableHead => TagKind::TableHead,
            MdTagEnd::TableRow => TagKind::TableRow,
            MdTagEnd::TableCell => TagKind::TableCell,
            MdTagEnd::Emphasis => TagKind::Emphasis,
            MdTagEnd::Strong => TagKind::Strong,
            MdTagEnd::Strikethrough => TagKind::Strikethrough,
            MdTagEnd::Superscript => TagKind::Super,
            MdTagEnd::Subscript => TagKind::Sub,
            MdTagEnd::Link => TagKind::Link,
            MdTagEnd::FootnoteDefinition => TagKind::Footnote,
            MdTagEnd::DefinitionList => TagKind::DefinitionList,
            MdTagEnd::DefinitionListTitle => TagKind::DefinitionTerm,
            MdTagEnd::DefinitionListDefinition => TagKind::DefinitionDetail,

            MdTagEnd::Image => {
                // Now we can: the alternative is complete, so emit the leaf.
                let alt = self.image_alt.take().unwrap_or_default();
                let source = self.pending_image.take().unwrap_or(Cow::Borrowed(""));
                self.emit(
                    Event::Image {
                        source: termdoc_core::ImageSource::Path(source),
                        alt: Cow::Owned(alt),
                        dims: None,
                    },
                    span,
                );
                return;
            }

            MdTagEnd::HtmlBlock | MdTagEnd::MetadataBlock(_) => return,
        };
        self.emit(Event::End(kind), span);
    }
}

/// `pulldown`'s `CowStr` to our `Cow<str>`, preserving the borrow where there is one.
fn cow(s: CowStr<'_>) -> Cow<'_, str> {
    match s {
        CowStr::Borrowed(b) => Cow::Borrowed(b),
        other => Cow::Owned(other.into_string()),
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn align(a: Alignment) -> Align {
    match a {
        Alignment::None => Align::None,
        Alignment::Left => Align::Left,
        Alignment::Center => Align::Center,
        Alignment::Right => Align::Right,
    }
}

fn admonition(k: BlockQuoteKind) -> AdmonitionKind {
    match k {
        BlockQuoteKind::Note => AdmonitionKind::Note,
        BlockQuoteKind::Tip => AdmonitionKind::Tip,
        BlockQuoteKind::Important => AdmonitionKind::Important,
        BlockQuoteKind::Warning => AdmonitionKind::Warning,
        BlockQuoteKind::Caution => AdmonitionKind::Caution,
    }
}

impl std::fmt::Debug for MarkdownEvents<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarkdownEvents")
            .field("queued", &self.queue.len())
            .field("list_depth", &self.lists.len())
            .finish()
    }
}
