//! Detection for the M0 formats.
//!
//! This is deliberately **partial**. The full detection engine from docs/DESIGN.md §4 —magic
//! bytes, intra-ZIP disambiguation, structural sniffing for JSON/YAML/TOML/CSV, `chardetng`—
//! lives in its own `termdoc-detect` crate and arrives in M1, once there are eight text
//! formats to disambiguate and the work is justified. With two formats, a separate crate
//! would be scaffolding without a purpose.
//!
//! What *is* here from the start is the **layered structure with confidence levels**, which
//! is what later allows adding layers without rewriting anything.

use termdoc_core::{Detection, Detector, FormatId, Source, confidence};

/// Extensions treated as Markdown.
const MARKDOWN_EXT: &[&str] = &["md", "markdown", "mdown", "mkd", "mdx"];
/// Extensions treated as logs.
const LOG_EXT: &[&str] = &["log"];
/// Extensions that are explicitly plain text.
const TEXT_EXT: &[&str] = &["txt", "text", "rst", "asc"];

#[derive(Debug, Default, Clone, Copy)]
pub struct TextDetector;

impl TextDetector {
    pub fn new() -> Self {
        TextDetector
    }
}

impl Detector for TextDetector {
    fn sniff(&self, src: &Source) -> Option<Detection> {
        // A binary is not this detector's business.
        if src.looks_binary() {
            return None;
        }

        // Layer: the extension. Text formats have no magic bytes, so here the extension is
        // a legitimate signal rather than a last resort.
        if let Some(ext) = extension(src) {
            if MARKDOWN_EXT.contains(&ext.as_str()) {
                return Some(Detection::new(
                    FormatId::Markdown,
                    confidence::EXTENSION,
                    format!(".{ext} extension"),
                ));
            }
            if LOG_EXT.contains(&ext.as_str()) {
                return Some(Detection::new(
                    FormatId::Log,
                    confidence::EXTENSION,
                    format!(".{ext} extension"),
                ));
            }
            if TEXT_EXT.contains(&ext.as_str()) {
                return Some(Detection::new(
                    FormatId::PlainText,
                    confidence::EXTENSION,
                    format!(".{ext} extension"),
                ));
            }
        }

        // Layer: structural sniffing. This is the only route available for stdin, where
        // there is no name at all.
        let probe = src.probe();
        let text = std::str::from_utf8(probe).unwrap_or_else(|e| {
            // The probe may cut a multi-byte character in half; use the valid part rather
            // than giving up on the sniff.
            std::str::from_utf8(&probe[..e.valid_up_to()]).unwrap_or("")
        });

        if let Some(reason) = markdown_markers(text) {
            return Some(Detection::new(
                FormatId::Markdown,
                confidence::HEURISTIC,
                reason,
            ));
        }

        None
    }
}

fn extension(src: &Source) -> Option<String> {
    src.path()?
        .extension()?
        .to_str()
        .map(|e| e.to_ascii_lowercase())
}

/// Unambiguous Markdown markers at the start of a line.
///
/// Column 0 is required because a `#` mid-line is a comment or a hash, not a heading. A
/// single marker is enough: a README starting with `# Title` leaves no room for doubt.
fn markdown_markers(text: &str) -> Option<String> {
    for line in text.lines().take(200) {
        // ATX heading: one to six `#` followed by a space.
        let hashes = line.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') {
            return Some("ATX heading in column 0".to_string());
        }
        // Code fence.
        if line.starts_with("```") || line.starts_with("~~~") {
            return Some("code fence".to_string());
        }
        // GFM table: a separator row is a very reliable marker.
        if line.starts_with('|') && line.contains("---") {
            return Some("GFM table separator".to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sniff_bytes(content: &str) -> Option<Detection> {
        TextDetector.sniff(&Source::from_bytes("<stdin>", content))
    }

    #[test]
    fn detects_markdown_by_heading_without_a_filename() {
        // The `cat README.md | termdoc` case: there is no extension to lean on.
        let d = sniff_bytes("# Title\n\ntext").expect("should detect");
        assert_eq!(d.format, FormatId::Markdown);
        assert_eq!(d.confidence, confidence::HEURISTIC);
    }

    #[test]
    fn detects_markdown_by_code_fence() {
        let d = sniff_bytes("text\n\n```rust\nfn main() {}\n```\n").unwrap();
        assert_eq!(d.format, FormatId::Markdown);
    }

    #[test]
    fn a_mid_line_hash_is_not_a_heading() {
        // Regression: `let x = 1; # comment` must not turn a script into Markdown.
        assert!(sniff_bytes("code = 1  # comment\nanother line\n").is_none());
    }

    #[test]
    fn seven_hashes_are_not_a_heading() {
        assert!(sniff_bytes("####### not valid\n").is_none());
    }

    #[test]
    fn a_hash_without_a_space_is_not_a_heading() {
        assert!(sniff_bytes("#hashtag\n").is_none());
    }

    #[test]
    fn plain_text_is_not_claimed() {
        // Returning `None` is correct: the `Registry` applies the text fallback.
        assert!(sniff_bytes("just normal text\nwith no markers\n").is_none());
    }

    #[test]
    fn binaries_are_not_claimed() {
        assert!(
            TextDetector
                .sniff(&Source::from_bytes("x", vec![b'#', b' ', 0, 1]))
                .is_none()
        );
    }

    #[test]
    fn a_prefix_cut_mid_character_does_not_break_the_sniff() {
        // The probe can split a multi-byte character; the sniff must still work.
        let mut bytes = b"# Title\n".to_vec();
        bytes.extend_from_slice(&[0xE2, 0x82]); // an incomplete "€"
        let d = TextDetector
            .sniff(&Source::from_bytes("x", bytes))
            .expect("should detect despite the cut");
        assert_eq!(d.format, FormatId::Markdown);
    }
}
