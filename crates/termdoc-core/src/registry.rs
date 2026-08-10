//! The registry of readers and detectors.
//!
//! The key point of the design (docs/DESIGN.md §2.4): an external plugin is wrapped in a
//! proxy that implements `DocumentReader`, so the core never has an "is this a plugin?"
//! branch. One code path.

use std::sync::Arc;

use crate::{Detection, Detector, DocumentReader, FormatId, Source, confidence};

#[derive(Default)]
pub struct Registry {
    readers: Vec<Arc<dyn DocumentReader>>,
    detectors: Vec<Arc<dyn Detector>>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("formats", &self.formats())
            .field("detectors", &self.detectors.len())
            .finish()
    }
}

impl Registry {
    pub fn new() -> Self {
        Registry::default()
    }

    pub fn register_reader(&mut self, reader: Arc<dyn DocumentReader>) -> &mut Self {
        self.readers.push(reader);
        self
    }

    pub fn register_detector(&mut self, detector: Arc<dyn Detector>) -> &mut Self {
        self.detectors.push(detector);
        self
    }

    /// Finds the reader that handles a format. The last one registered wins, so a plugin
    /// can deliberately replace a built-in.
    pub fn reader_for(&self, format: FormatId) -> Option<&dyn DocumentReader> {
        self.readers
            .iter()
            .rev()
            .find(|r| r.id() == format)
            .map(|r| r.as_ref())
    }

    pub fn formats(&self) -> Vec<FormatId> {
        let mut v: Vec<_> = self.readers.iter().map(|r| r.id()).collect();
        v.dedup();
        v
    }

    /// Runs every detector and returns the most confident result.
    ///
    /// Ties go to whichever was registered later: plugins register after the built-ins,
    /// so they can claim a format at equal confidence.
    pub fn detect(&self, src: &Source) -> Option<Detection> {
        let mut best: Option<Detection> = None;
        for d in &self.detectors {
            if let Some(candidate) = d.sniff(src) {
                let better = match &best {
                    None => true,
                    Some(b) => candidate.confidence >= b.confidence,
                };
                if better {
                    best = Some(candidate);
                }
            }
        }
        best
    }

    /// Every candidate, ordered by descending confidence. This is what `--explain`
    /// prints: seeing what was rejected is half the useful information.
    pub fn detect_all(&self, src: &Source) -> Vec<Detection> {
        let mut all: Vec<_> = self.detectors.iter().filter_map(|d| d.sniff(src)).collect();
        all.sort_by_key(|d| std::cmp::Reverse(d.confidence));
        all
    }

    /// Detection with the final safety net: if nobody claims the source, decide between
    /// text and binary. Never returns `None`, because "I don't know what this is" is not
    /// an acceptable answer from a universal viewer.
    pub fn detect_or_fallback(&self, src: &Source) -> Detection {
        if let Some(d) = self.detect(src) {
            return d;
        }
        if src.looks_binary() {
            Detection::new(
                FormatId::Binary,
                confidence::FALLBACK,
                "no matches; there is a NUL byte in the prefix",
            )
        } else {
            Detection::new(
                FormatId::PlainText,
                confidence::FALLBACK,
                "no matches; the prefix is text",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Events, ReadContext, Result};

    struct Fake(FormatId);

    impl DocumentReader for Fake {
        fn id(&self) -> FormatId {
            self.0
        }
        fn read<'a>(&self, _src: &'a Source, _ctx: &ReadContext) -> Result<Events<'a>> {
            Ok(Box::new(std::iter::empty()))
        }
    }

    struct Sniffer(FormatId, u8);

    impl Detector for Sniffer {
        fn sniff(&self, _src: &Source) -> Option<Detection> {
            Some(Detection::new(self.0, self.1, "test"))
        }
    }

    #[test]
    fn the_last_registered_reader_wins() {
        let mut r = Registry::new();
        r.register_reader(Arc::new(Fake(FormatId::Markdown)));
        r.register_reader(Arc::new(Fake(FormatId::Markdown)));
        // Registering the same format twice must not break resolution: it is the
        // mechanism by which a plugin replaces a built-in.
        assert!(r.reader_for(FormatId::Markdown).is_some());
        assert!(r.reader_for(FormatId::Pdf).is_none());
    }

    #[test]
    fn detect_picks_the_highest_confidence() {
        let mut r = Registry::new();
        r.register_detector(Arc::new(Sniffer(FormatId::PlainText, 30)));
        r.register_detector(Arc::new(Sniffer(FormatId::Markdown, 70)));
        r.register_detector(Arc::new(Sniffer(FormatId::Csv, 50)));
        assert_eq!(
            r.detect(&Source::from_bytes("t", "x")).unwrap().format,
            FormatId::Markdown
        );
    }

    #[test]
    fn detect_all_sorts_descending() {
        let mut r = Registry::new();
        r.register_detector(Arc::new(Sniffer(FormatId::PlainText, 30)));
        r.register_detector(Arc::new(Sniffer(FormatId::Markdown, 70)));
        let all = r.detect_all(&Source::from_bytes("t", "x"));
        assert_eq!(all.len(), 2);
        assert!(all[0].confidence >= all[1].confidence);
    }

    #[test]
    fn fallback_distinguishes_text_from_binary() {
        let r = Registry::new();
        assert_eq!(
            r.detect_or_fallback(&Source::from_bytes("t", "hello"))
                .format,
            FormatId::PlainText
        );
        assert_eq!(
            r.detect_or_fallback(&Source::from_bytes("t", vec![0, 1, 2]))
                .format,
            FormatId::Binary
        );
    }
}
