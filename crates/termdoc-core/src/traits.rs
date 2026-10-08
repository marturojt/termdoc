//! The interfaces that decouple the pipeline.
//!
//! Every stage of docs/DESIGN.md §2.1 is a trait here. Readers live in crates that depend
//! only on this one; backends live in crates that cannot see readers. Cargo's dependency
//! graph is what keeps that separation from eroding.

use crate::{Detection, Events, FormatId, Line, Result, Source};

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
#[derive(Clone, Debug, Default)]
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
