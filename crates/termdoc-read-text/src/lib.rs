//! Plain-text and Markdown readers.
//!
//! LAYERING INVARIANT: depends **only** on `termdoc-core`. A reader that could see a backend
//! would, sooner or later, start emitting ANSI on its own, and with that the ability to add
//! new backends is gone. The restriction is not a recommendation: it is in this crate's
//! `Cargo.toml`.

#![warn(missing_debug_implementations)]

mod detect;
mod markdown;
mod text;

pub use detect::TextDetector;
pub use markdown::MarkdownReader;
pub use text::TextReader;

use std::sync::Arc;

use termdoc_core::Registry;

/// Registers this crate's readers and detector.
///
/// Every reader crate exposes its own `register`, so the CLI composes the registry without
/// knowing the details of any of them, and a build with fewer features simply calls fewer of
/// these functions.
pub fn register(registry: &mut Registry) {
    registry.register_reader(Arc::new(TextReader::plain()));
    registry.register_reader(Arc::new(TextReader::log()));
    registry.register_reader(Arc::new(MarkdownReader::new()));
    registry.register_detector(Arc::new(TextDetector::new()));
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
    fn a_readme_on_stdin_reaches_the_markdown_reader() {
        // The full chain: sniff with no filename → format → reader.
        let mut r = Registry::new();
        register(&mut r);
        let src = Source::from_bytes("<stdin>", "# Title\n\nparagraph\n");
        let detection = r.detect_or_fallback(&src);
        assert_eq!(detection.format, FormatId::Markdown);
        assert!(r.reader_for(detection.format).is_some());
    }

    #[test]
    fn text_without_markers_falls_back_to_plain_text() {
        let mut r = Registry::new();
        register(&mut r);
        let src = Source::from_bytes("<stdin>", "just text\n");
        assert_eq!(r.detect_or_fallback(&src).format, FormatId::PlainText);
    }
}
