//! Readers for structured data formats.
//!
//! The four formats in this crate look like one job and are not. Their parsers have opposite
//! properties, and pretending otherwise would mean either paying for streaming where it buys
//! nothing or materialising where it cannot be afforded (docs/HANDOFF.md §6.1):
//!
//! | format | approach | why |
//! |---|---|---|
//! | JSON | materialise, with a size ceiling | `serde_json` builds a tree; the ceiling bounds it |
//! | TOML | materialise, no apology | config files; streaming buys nothing |
//! | XML | stream | `quick-xml` is already a pull parser that borrows |
//! | YAML | line-by-line highlighter | a parser's events drop the comments; see `yaml.rs` |
//!
//! JSON and YAML are implemented so far.
//!
//! # Layering
//!
//! This crate depends on `termdoc-core` and nothing else. A reader describes a document; it
//! does not decide how it looks. If something here wanted the terminal width or a colour, the
//! answer would belong in the layout or the theme.
//!
//! No detectors are registered: naming a format belongs to `termdoc-detect`, so that detection
//! order does not depend on which readers a build happens to contain.

mod json;
mod yaml;

pub use json::{JsonReader, MAX_MATERIALISED};
pub use yaml::YamlReader;

use std::sync::Arc;
use termdoc_core::Registry;

/// Registers every reader in this crate.
pub fn register(registry: &mut Registry) {
    registry.register_reader(Arc::new(JsonReader::new()));
    registry.register_reader(Arc::new(YamlReader::new()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use termdoc_core::FormatId;

    #[test]
    fn register_makes_json_resolvable() {
        let mut r = Registry::new();
        register(&mut r);
        assert!(r.reader_for(FormatId::Json).is_some());
    }

    #[test]
    fn register_makes_yaml_resolvable() {
        let mut r = Registry::new();
        register(&mut r);
        assert!(r.reader_for(FormatId::Yaml).is_some());
    }

    #[test]
    fn registering_readers_adds_no_detectors() {
        // Detection order must depend on the detection layer alone, never on which readers
        // happen to be compiled in.
        let mut r = Registry::new();
        register(&mut r);
        assert!(
            r.detect_all(&termdoc_core::Source::from_bytes("x.json", b"{}".to_vec()))
                .is_empty(),
            "a reader crate must register no detectors"
        );
    }
}
