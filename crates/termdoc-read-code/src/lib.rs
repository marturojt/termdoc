//! Syntax highlighting for termdoc.
//!
//! Two things, one engine ([`engine`]):
//!
//! - [`CodeReader`] reads a source file (`FormatId::SourceCode`) and highlights it.
//! - [`CodeBlockHighlighter`] is a [`Transform`](termdoc_core::Transform) that highlights the
//!   fenced code blocks of any document, Markdown first of all.
//!
//! Both say what a span *is* — a keyword, a string, a comment — with `Tag::Token`; the theme
//! decides how that looks. See `engine.rs` for how the grammars are run, what that costs
//! (measured), and the ceiling that keeps a large file from becoming a hang.
//!
//! # Layering
//!
//! This crate depends on `termdoc-core` and nothing else, like every reader. No detectors are
//! registered: naming a format belongs to `termdoc-detect`.

mod engine;
mod reader;
mod transform;

pub use engine::{MAX_HIGHLIGHTED_BYTES, MAX_LINE};
pub use reader::CodeReader;
pub use transform::CodeBlockHighlighter;

use std::sync::Arc;
use termdoc_core::Registry;

/// Registers the source-file reader. The block highlighter is a transform, which the caller
/// applies to whichever stream it wants highlighted.
pub fn register(registry: &mut Registry) {
    registry.register_reader(Arc::new(CodeReader::new()));
}
