//! Plain-text and Markdown readers.
//!
//! LAYERING INVARIANT: depends **only** on `termdoc-core`. A reader that could see a backend
//! would, sooner or later, start emitting ANSI on its own, and with that the ability to add
//! new backends is gone. The restriction is not a recommendation: it is in this crate's
//! `Cargo.toml`.

#![warn(missing_debug_implementations)]

mod markdown;
mod text;

pub use markdown::MarkdownReader;
pub use text::TextReader;

use std::sync::Arc;

use termdoc_core::Registry;

/// Registers this crate's readers.
///
/// Every reader crate exposes its own `register`, so the CLI composes the registry without
/// knowing the details of any of them, and a build with fewer features simply calls fewer of
/// these functions.
///
/// No detector is registered here: deciding *what* a document is belongs to `termdoc-detect`,
/// which owns the layered engine. A reader crate that also guessed formats would make the
/// detection order depend on which readers happen to be compiled in.
pub fn register(registry: &mut Registry) {
    registry.register_reader(Arc::new(TextReader::plain()));
    registry.register_reader(Arc::new(TextReader::log()));
    registry.register_reader(Arc::new(MarkdownReader::new()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use termdoc_core::{FormatId, Source};

    #[test]
    fn register_makes_the_m0_formats_resolvable() {
        let mut r = Registry::new();
        register(&mut r);
        for f in [FormatId::PlainText, FormatId::Markdown, FormatId::Log] {
            assert!(r.reader_for(f).is_some(), "{f} has no reader");
        }
    }

    #[test]
    fn registering_readers_adds_no_detectors() {
        // The layering this crate is responsible for: it says how to read, never what things
        // are. If a reader crate registered detectors, the detection order would depend on
        // which readers were compiled in.
        let mut r = Registry::new();
        register(&mut r);
        let src = Source::from_bytes("<stdin>", "# Title\n");
        assert!(
            r.detect(&src).is_none(),
            "no detector should come from this crate"
        );
    }
}
