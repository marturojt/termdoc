//! termdoc's shared vocabulary: the document model, the pipeline traits, and input
//! sources.
//!
//! This crate **depends on no other termdoc crate**. Readers depend only on it; backends
//! cannot see readers either. That separation is not a convention but Cargo's dependency
//! graph, verified by `crates/termdoc-cli/tests/layering.rs`.
//!
//! The full architecture lives in `docs/DESIGN.md`.

#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_debug_implementations)]

mod error;
mod event;
mod format;
mod line;
mod registry;
mod source;
mod traits;

pub use error::{Error, Result, exit};
pub use event::{
    AdmonitionKind, Align, BreakKind, Diagnostic, Event, Events, ImageSource, Marker, Metadata,
    Severity, Span, Spanned, Tag, TagKind,
};
pub use format::{Confidence, Detection, FormatId, confidence};
pub use line::{Color, Line, NamedColor, Segment, Style};
pub use registry::Registry;
pub use source::{Origin, PROBE_SIZE, Source};
pub use traits::{
    Backend, BackendCaps, Detector, DocumentReader, ReadContext, ReaderCaps, Transform,
};
