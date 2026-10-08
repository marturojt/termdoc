//! The interfaces that decouple the pipeline.
//!
//! Every stage of docs/DESIGN.md §2.1 is a trait here. Readers live in crates that depend
//! only on this one; backends live in crates that cannot see readers. Cargo's dependency
//! graph is what keeps that separation from eroding.

use crate::stream::LineStream;
use crate::{Detection, Error, Events, FormatId, Line, Result, Source};

/// Turns bytes into the internal model. This is what the original design called a
/// "Renderer": it produces the document, it does not paint it.
pub trait DocumentReader: Send + Sync {
    fn id(&self) -> FormatId;

    /// Reads `src` and returns the stream. The `'a` lifetime ties the events to the
    /// source, which is what lets `Cow::Borrowed` borrow from the `mmap` without copying.
    fn read<'a>(&self, src: &'a Source, ctx: &ReadContext) -> Result<Events<'a>>;

    fn capabilities(&self) -> ReaderCaps {
        ReaderCaps::default()
    }

    /// Whether this reader can read an input line by line as it arrives, without waiting for
    /// its end. True for formats whose lines stand alone — plain text, logs — and false (the
    /// default) for anything that needs the whole document, which is then read to the end first.
    fn streams_input(&self) -> bool {
        false
    }

    /// Reads an input that is still arriving. Only called when [`streams_input`] says it can.
    ///
    /// [`streams_input`]: DocumentReader::streams_input
    ///
    /// The events own what they carry, so they can stand wherever events borrowing from a
    /// source can: the lifetime is the caller's to choose.
    fn read_stream<'a>(&self, stream: LineStream, ctx: &ReadContext) -> Result<Events<'a>> {
        let _ = (stream, ctx);
        Err(Error::Unsupported(format!(
            "the {} reader cannot read from a stream",
            self.id()
        )))
    }
}

/// What a reader can do. The CLI consults this to know, for instance, whether offering
/// `--page` makes any sense.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReaderCaps {
    /// Emits events without materializing the whole document.
    pub streaming: bool,
    /// Has navigable pages.
    pub paginated: bool,
    /// Can contribute metadata.
    pub metadata: bool,
}

/// Options a reader needs to know about. Deliberately excludes width and color: those
/// belong to the layout and the backend, and leaking them here would re-couple the
/// stages.
#[derive(Clone, Debug)]
pub struct ReadContext {
    /// Page or section range requested with `--page`.
    pub page_range: Option<(u32, u32)>,
    /// Encoding forced with `--encoding`.
    pub encoding: Option<String>,
    /// Metadata only: lets the reader skip the body.
    pub metadata_only: bool,
    /// The field delimiter of delimited text (CSV, TSV), as detection resolved it.
    ///
    /// A reader may not depend on `termdoc-detect`, so what detection knows reaches it here.
    /// `None` means nobody knows, and the reader assumes a comma.
    pub delimiter: Option<u8>,
    /// Whether the first record of delimited text is a header. `None` lets the reader decide.
    pub header: Option<bool>,
    /// Whether the output can carry colour at all. A reader that spends effort telling roles
    /// apart (syntax highlighting) can skip it when this is `false`, because the text that comes
    /// out is the same either way. Defaults to `true`: the safe answer is to describe.
    pub styled: bool,
}

impl Default for ReadContext {
    fn default() -> Self {
        ReadContext {
            page_range: None,
            encoding: None,
            metadata_only: false,
            delimiter: None,
            header: None,
            styled: true,
        }
    }
}

/// Proposes a format based on the source's prefix.
pub trait Detector: Send + Sync {
    fn sniff(&self, src: &Source) -> Option<Detection>;
}

/// Transforms the stream. These chain for free because they are iterator adapters.
pub trait Transform: Send + Sync {
    fn apply<'a>(&self, events: Events<'a>) -> Events<'a>;
}

/// Writes laid-out lines. Sees neither events nor readers.
pub trait Backend {
    fn caps(&self) -> BackendCaps;

    fn begin(&mut self, out: &mut dyn std::io::Write) -> Result<()> {
        let _ = out;
        Ok(())
    }

    fn write_line(&mut self, line: &Line<'_>, out: &mut dyn std::io::Write) -> Result<()>;

    fn finish(&mut self, out: &mut dyn std::io::Write) -> Result<()> {
        let _ = out;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackendCaps {
    pub styled: bool,
    pub hyperlinks: bool,
    pub graphics: bool,
}
