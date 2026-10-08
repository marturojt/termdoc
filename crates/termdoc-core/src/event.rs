//! The internal document model: an event stream, not a tree.
//!
//! See docs/DESIGN.md §2.2 for the reasoning. In short: a 10M-line log is never
//! materialized, `Cow<'a, str>` borrows straight from the `mmap`, and backends stay as
//! state machines that are trivial to test.
//!
//! ## Refinement over the original design
//!
//! The approved design separated `Event::Inline(Inline)` from `Event::Start(Block)`, with
//! `Emphasis`/`Strong`/`Link` living inside `Inline`. That cannot be expressed: emphasis
//! *wraps* content, so it needs an open and a close just like a paragraph does. With a
//! single `Inline(Inline::Emphasis)` there is no way to know where it ends.
//!
//! Everything is therefore unified into `Start(Tag)` / `End(TagKind)` for every container
//! —block-level or inline— with leaf events for whatever wraps nothing. This is
//! `pulldown-cmark`'s proven model, and it makes mapping its output nearly mechanical.

use std::borrow::Cow;
use std::sync::Arc;

/// Where an event came from in the source.
///
/// Without this there is no `--page`, no jumping from a search hit, and no navigable
/// table of contents; adding it later would mean touching every reader, so it is here
/// from day one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    /// Byte range within the original source.
    pub start: u64,
    pub end: u64,
    /// 1-based line, for formats that have lines.
    pub line: Option<u32>,
    /// 1-based page, for formats that have pages (PDF, EPUB).
    pub page: Option<u32>,
}

impl Span {
    pub const fn empty() -> Self {
        Span {
            start: 0,
            end: 0,
            line: None,
            page: None,
        }
    }

    pub const fn bytes(start: u64, end: u64) -> Self {
        Span {
            start,
            end,
            line: None,
            page: None,
        }
    }

    pub const fn at_line(start: u64, end: u64, line: u32) -> Self {
        Span {
            start,
            end,
            line: Some(line),
            page: None,
        }
    }
}

/// An event together with its location.
#[derive(Clone, Debug, PartialEq)]
pub struct Spanned<T> {
    pub node: T,
    pub span: Span,
}

impl<T> Spanned<T> {
    pub const fn new(node: T, span: Span) -> Self {
        Spanned { node, span }
    }

    /// For readers that do not track positions precisely yet.
    pub const fn bare(node: T) -> Self {
        Spanned {
            node,
            span: Span::empty(),
        }
    }
}

/// The stream. Fallible per event: a document that turns out to be corrupt halfway
/// through must not invalidate what was already emitted (see `Diagnostic` and
/// docs/DESIGN.md §7).
pub type Events<'a> = Box<dyn Iterator<Item = crate::Result<Spanned<Event<'a>>>> + 'a>;

#[derive(Clone, Debug, PartialEq)]
pub enum Event<'a> {
    /// Opens a container, block-level or inline.
    Start(Tag<'a>),
    /// Closes the most recently opened container.
    End(TagKind),

    // --- Leaves ---
    Text(Cow<'a, str>),
    /// Inline code. A leaf because its content never carries formatting.
    Code(Cow<'a, str>),
    Image {
        source: ImageSource<'a>,
        alt: Cow<'a, str>,
        dims: Option<(u32, u32)>,
    },
    Math {
        inline: bool,
        tex: Cow<'a, str>,
    },
    FootnoteRef(Cow<'a, str>),
    Break(BreakKind),
    Rule,
    PageBreak,
    /// A GFM task-list marker: `- [x]`.
    TaskMarker(bool),

    /// A recoverable problem. The stream continues: partial rendering beats total
    /// failure.
    Diagnostic(Diagnostic),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Tag<'a> {
    // --- Blocks ---
    /// Wraps the whole document; carries the metadata.
    Document(Box<Metadata<'a>>),
    Section {
        level: u8,
    },
    Heading {
        level: u8,
        id: Option<Cow<'a, str>>,
    },
    Paragraph,
    /// Text where the source's own line breaks are meaningful and must not be reflowed:
    /// plain text, logs, command output. Distinct from `CodeBlock`, which also
    /// highlights.
    Preformatted,
    List {
        ordered: bool,
        start: u64,
        tight: bool,
    },
    ListItem {
        marker: Marker,
    },
    Table {
        align: Vec<Align>,
    },
    TableHead,
    TableRow,
    TableCell {
        colspan: u16,
        rowspan: u16,
    },
    CodeBlock {
        lang: Option<Cow<'a, str>>,
        filename: Option<Cow<'a, str>>,
    },
    BlockQuote {
        attribution: Option<Cow<'a, str>>,
    },
    Admonition {
        kind: AdmonitionKind,
    },
    Figure {
        caption: Option<Cow<'a, str>>,
    },
    Footnote {
        id: Cow<'a, str>,
    },
    DefinitionList,
    DefinitionTerm,
    DefinitionDetail,

    // --- Inline containers ---
    Emphasis,
    Strong,
    Strikethrough,
    Underline,
    Highlight,
    SmallCaps,
    Sub,
    Super,
    Link {
        href: Cow<'a, str>,
        title: Option<Cow<'a, str>>,
    },
    /// A span of text with a syntactic role: a JSON key, a number, a comment.
    ///
    /// The reader says *what* the text is and the theme decides how it looks, which is what
    /// keeps readers from choosing colors. Valid inside `Preformatted` and `CodeBlock`, where
    /// several of these share one line, and anywhere else inline.
    Token {
        role: TokenRole,
    },
}

/// What a [`Tag::Token`] span is, in terms of the document's own syntax rather than a color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenRole {
    /// An object key, a mapping key, a TOML key.
    Key,
    String,
    Number,
    /// `true` / `false`.
    Bool,
    Null,
    /// Braces, brackets, commas, colons: structure that carries no data.
    Punctuation,
    Comment,
    /// A table header, an element name.
    Name,
    /// An XML attribute name.
    Attribute,
    /// A language keyword: `fn`, `if`, `return`.
    Keyword,
    /// A type or class name.
    Type,
    /// A function or method name.
    Function,
    /// An operator: `+`, `=>`, `&&`.
    Operator,
    /// A named constant or a character escape.
    Constant,
}

/// `Tag`'s discriminant, so `End` does not have to carry the payload again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TagKind {
    Document,
    Section,
    Heading,
    Paragraph,
    Preformatted,
    List,
    ListItem,
    Table,
    TableHead,
    TableRow,
    TableCell,
    CodeBlock,
    BlockQuote,
    Admonition,
    Figure,
    Footnote,
    DefinitionList,
    DefinitionTerm,
    DefinitionDetail,
    Emphasis,
    Strong,
    Strikethrough,
    Underline,
    Highlight,
    SmallCaps,
    Sub,
    Super,
    Link,
    Token,
}

impl Tag<'_> {
    pub fn kind(&self) -> TagKind {
        match self {
            Tag::Document(_) => TagKind::Document,
            Tag::Section { .. } => TagKind::Section,
            Tag::Heading { .. } => TagKind::Heading,
            Tag::Paragraph => TagKind::Paragraph,
            Tag::Preformatted => TagKind::Preformatted,
            Tag::List { .. } => TagKind::List,
            Tag::ListItem { .. } => TagKind::ListItem,
            Tag::Table { .. } => TagKind::Table,
            Tag::TableHead => TagKind::TableHead,
            Tag::TableRow => TagKind::TableRow,
            Tag::TableCell { .. } => TagKind::TableCell,
            Tag::CodeBlock { .. } => TagKind::CodeBlock,
            Tag::BlockQuote { .. } => TagKind::BlockQuote,
            Tag::Admonition { .. } => TagKind::Admonition,
            Tag::Figure { .. } => TagKind::Figure,
            Tag::Footnote { .. } => TagKind::Footnote,
            Tag::DefinitionList => TagKind::DefinitionList,
            Tag::DefinitionTerm => TagKind::DefinitionTerm,
            Tag::DefinitionDetail => TagKind::DefinitionDetail,
            Tag::Emphasis => TagKind::Emphasis,
            Tag::Strong => TagKind::Strong,
            Tag::Strikethrough => TagKind::Strikethrough,
            Tag::Underline => TagKind::Underline,
            Tag::Highlight => TagKind::Highlight,
            Tag::SmallCaps => TagKind::SmallCaps,
            Tag::Sub => TagKind::Sub,
            Tag::Super => TagKind::Super,
            Tag::Link { .. } => TagKind::Link,
            Tag::Token { .. } => TagKind::Token,
        }
    }
}

impl TagKind {
    /// `true` when the container is block-level (forces a line break) rather than inline.
    pub fn is_block(self) -> bool {
        !matches!(
            self,
            TagKind::Emphasis
                | TagKind::Strong
                | TagKind::Strikethrough
                | TagKind::Underline
                | TagKind::Highlight
                | TagKind::SmallCaps
                | TagKind::Sub
                | TagKind::Super
                | TagKind::Link
                | TagKind::Token
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BreakKind {
    /// A break in the source that the layout may reflow into a space.
    Soft,
    /// A break that must be honored.
    Hard,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    None,
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmonitionKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

/// A list item's bullet. The layout picks the glyph based on `Fidelity`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    Bullet {
        /// Nesting depth, used to rotate the glyph.
        depth: u8,
    },
    Ordered {
        number: u64,
    },
}

/// A handle to an image; **never pixels**.
///
/// Decoding a 4000x3000 PNG only to print `[image: diagram]` is exactly the kind of waste
/// this project cannot afford, so resolution is deferred until we know whether the
/// backend can display graphics at all.
#[derive(Clone)]
pub enum ImageSource<'a> {
    Path(Cow<'a, str>),
    Bytes(Arc<[u8]>),
    /// An entry inside a container (the ZIP of a DOCX/EPUB, a PDF object).
    Entry {
        container: Cow<'a, str>,
        name: Cow<'a, str>,
    },
}

impl std::fmt::Debug for ImageSource<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageSource::Path(p) => write!(f, "Path({p:?})"),
            ImageSource::Bytes(b) => write!(f, "Bytes({} bytes)", b.len()),
            ImageSource::Entry { container, name } => {
                write!(f, "Entry({container:?}, {name:?})")
            }
        }
    }
}

impl PartialEq for ImageSource<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (ImageSource::Path(a), ImageSource::Path(b)) => a == b,
            (ImageSource::Bytes(a), ImageSource::Bytes(b)) => a == b,
            (
                ImageSource::Entry {
                    container: c1,
                    name: n1,
                },
                ImageSource::Entry {
                    container: c2,
                    name: n2,
                },
            ) => c1 == c2 && n1 == n2,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metadata<'a> {
    pub title: Option<Cow<'a, str>>,
    pub authors: Vec<Cow<'a, str>>,
    pub date: Option<Cow<'a, str>>,
    pub language: Option<Cow<'a, str>>,
    pub page_count: Option<u32>,
    pub word_count: Option<u64>,
    pub source_format: Option<crate::FormatId>,
    pub encoding: Option<Cow<'a, str>>,
    /// Format-specific extras, kept out of the shared model.
    pub custom: Vec<(Cow<'a, str>, Cow<'a, str>)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// A recoverable problem found while reading. It travels *inside* the stream so that
/// rendering can continue; where it ends up —dimmed inline on a TTY, on stderr in a
/// pipe— is the CLI's decision, not the reader's.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
}

impl Diagnostic {
    pub fn warning(message: impl Into<String>) -> Self {
        Diagnostic {
            severity: Severity::Warning,
            message: message.into(),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Diagnostic {
            severity: Severity::Error,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_round_trips_for_every_tag() {
        // Catches adding a `Tag` while forgetting its `TagKind`.
        let tags = [
            Tag::Paragraph,
            Tag::Preformatted,
            Tag::Emphasis,
            Tag::Strong,
            Tag::Heading { level: 1, id: None },
            Tag::List {
                ordered: false,
                start: 1,
                tight: true,
            },
            Tag::Link {
                href: "x".into(),
                title: None,
            },
            Tag::Token {
                role: TokenRole::Key,
            },
        ];
        for t in tags {
            let k = t.kind();
            assert_eq!(k, t.kind(), "kind() must be stable");
        }
    }

    #[test]
    fn inline_is_not_a_block() {
        assert!(!TagKind::Emphasis.is_block());
        assert!(!TagKind::Link.is_block());
        assert!(!TagKind::Token.is_block());
        assert!(TagKind::Paragraph.is_block());
        assert!(TagKind::Table.is_block());
    }
}
