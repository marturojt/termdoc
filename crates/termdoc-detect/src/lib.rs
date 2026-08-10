//! Layered format and encoding detection.
//!
//! The engine described in docs/DESIGN.md §4. Each layer is a separate `Detector` with its own
//! confidence level, and they are registered in order so `Registry::detect_all` — which is what
//! `--explain` prints — naturally shows every layer's opinion, including the ones that lost.
//!
//! Confidence order, and why:
//!
//! | Layer | Confidence | Why there |
//! |---|--:|---|
//! | Magic bytes | 90 | `%PDF-` is not a guess. Includes intra-ZIP disambiguation |
//! | Structural  | 70 | The prefix was actually checked (JSON, XML, TOML, CSV) |
//! | Extension   | 50 | Text formats have no magic; the filename is real evidence |
//! | Heuristic   | 30 | Markers and statistics (Markdown, YAML, logs) |
//!
//! LAYERING INVARIANT: depends only on `termdoc-core`. Detection answers "what is this?", never
//! "how does it look?".

#![warn(missing_debug_implementations)]

mod charset;
mod delimited;
mod extension;
mod magic;
mod structural;

pub use charset::{Charset, CharsetEvidence, detect as detect_charset, detect_bytes};
pub use delimited::{Delimited, detect as detect_delimiter};
pub use extension::language_for;

use std::sync::Arc;

use termdoc_core::{Detection, Detector, FormatId, Registry, Source};

/// Registers every detection layer.
///
/// The order is the registration order, which matters for ties: `Registry::detect` keeps the
/// later one at equal confidence.
pub fn register(registry: &mut Registry) {
    registry.register_detector(Arc::new(MagicDetector));
    registry.register_detector(Arc::new(StructuralDetector));
    registry.register_detector(Arc::new(ExtensionDetector));
    registry.register_detector(Arc::new(HeuristicDetector));
}

/// Layer 1: magic bytes and intra-ZIP disambiguation.
#[derive(Debug, Default, Clone, Copy)]
pub struct MagicDetector;

impl Detector for MagicDetector {
    fn sniff(&self, src: &Source) -> Option<Detection> {
        magic::sniff(src)
    }
}

/// Layer 2: structural sniffing over the prefix.
#[derive(Debug, Default, Clone, Copy)]
pub struct StructuralDetector;

impl Detector for StructuralDetector {
    fn sniff(&self, src: &Source) -> Option<Detection> {
        // A binary is not this layer's business; magic bytes already had their say.
        if src.looks_binary() {
            return None;
        }
        let text = probe_text(src);

        // A shebang is checked first: it identifies the interpreter outright, and a script
        // often has no extension at all.
        if let Some(d) = extension::sniff_shebang(text) {
            return Some(d);
        }
        structural::sniff(text)
    }
}

/// Layer 3: the filename.
#[derive(Debug, Default, Clone, Copy)]
pub struct ExtensionDetector;

impl Detector for ExtensionDetector {
    fn sniff(&self, src: &Source) -> Option<Detection> {
        extension::sniff(src)
    }
}

/// Layer 4: weak markers and statistics.
#[derive(Debug, Default, Clone, Copy)]
pub struct HeuristicDetector;

impl Detector for HeuristicDetector {
    fn sniff(&self, src: &Source) -> Option<Detection> {
        if src.looks_binary() {
            return None;
        }
        structural::sniff_heuristic(probe_text(src))
    }
}

/// The prefix as text, tolerating a cut in the middle of a character.
///
/// The probe boundary lands wherever it lands, so a multi-byte character can be sliced in half.
/// Using the valid part rather than giving up is what keeps a UTF-8 file from being misdetected
/// because of where 8 KB happened to end.
fn probe_text(src: &Source) -> &str {
    let probe = src.probe();
    match std::str::from_utf8(probe) {
        Ok(s) => s,
        Err(e) => std::str::from_utf8(&probe[..e.valid_up_to()]).unwrap_or(""),
    }
}

/// Detects the format and the encoding in a single pass, and configures the source.
///
/// This is the CLI's entry point. It exists so that "detect, then apply" cannot be got wrong
/// from the outside: `Source::set_encoding` requires `&mut`, so it has to happen before any
/// reader borrows the source, and doing it here makes that ordering structural.
///
/// `forced_encoding` comes from `--encoding` and overrides detection, because an explicit
/// request from the user outranks any guess of ours.
pub fn prepare(
    registry: &Registry,
    src: &mut Source,
    forced_encoding: Option<&str>,
) -> termdoc_core::Result<Detection> {
    let charset = charset::detect(src);

    match forced_encoding {
        Some(label) => src.set_encoding(label)?,
        None => {
            // UTF-32 is detected only so we can say so: `encoding_rs` does not implement it,
            // and silently decoding it as something else would produce garbage.
            if charset.label == "utf-32" {
                return Err(termdoc_core::Error::Encoding(
                    "UTF-32 is not supported; convert the file with `iconv -f UTF-32 -t UTF-8`"
                        .to_string(),
                ));
            }
            if charset.label != "utf-8" {
                src.set_encoding(charset.label)?;
            }
        }
    }

    let mut detection = registry.detect_or_fallback(src);
    detection.encoding = Some(if forced_encoding.is_some() {
        src.encoding_name()
    } else {
        charset.label
    });
    Ok(detection)
}

/// A description of how the encoding was decided, for `--explain`.
pub fn charset_reason(src: &Source) -> String {
    charset::detect(src).reason()
}

/// The formats this crate's detectors can name.
///
/// Used by `--explain` to distinguish "no detector recognized it" from "it was recognized but
/// there is no reader for it", which are very different problems for the user.
pub fn detectable_formats() -> &'static [FormatId] {
    &[
        FormatId::PlainText,
        FormatId::Markdown,
        FormatId::Log,
        FormatId::SourceCode,
        FormatId::Json,
        FormatId::Yaml,
        FormatId::Toml,
        FormatId::Xml,
        FormatId::Csv,
        FormatId::Html,
        FormatId::Pdf,
        FormatId::Docx,
        FormatId::Odt,
        FormatId::Rtf,
        FormatId::Epub,
        FormatId::Xlsx,
        FormatId::Pptx,
        FormatId::Binary,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use termdoc_core::confidence;

    fn registry() -> Registry {
        let mut r = Registry::new();
        register(&mut r);
        r
    }

    fn from_stdin(content: &str) -> Source {
        Source::from_bytes("<stdin>", content)
    }

    /// A `Source` backed by a real file, in a directory unique to this call.
    ///
    /// The uniqueness is not decoration: cargo runs tests in parallel, and two of them sharing
    /// a fixture path meant one could `open` the file in the instant the other had truncated
    /// it to zero bytes. The detection then fell back to plain text and the failure looked
    /// like a detection bug rather than a fixture race.
    fn named(name: &str, content: &str) -> Source {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("termdoc-detect-lib-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path: PathBuf = dir.join(name);
        std::fs::write(&path, content).unwrap();
        Source::open(&path).unwrap()
    }

    fn detect(src: &Source) -> Detection {
        registry().detect_or_fallback(src)
    }

    // ---------------- layer precedence

    #[test]
    fn content_outranks_the_extension_for_structured_data() {
        // A `.txt` file holding JSON: content is more reliable than the name.
        let d = detect(&named("data.txt", r#"{"a": 1, "b": 2}"#));
        assert_eq!(d.format, FormatId::Json);
        assert_eq!(d.confidence, confidence::STRUCTURAL);
    }

    #[test]
    fn the_extension_wins_where_content_is_ambiguous() {
        // This is the case the YAML rule protects. `key: value` lines are not distinctive, so
        // the `.yaml` extension is what decides.
        let d = detect(&named("config.yaml", "name: termdoc\nversion: 1\n"));
        assert_eq!(d.format, FormatId::Yaml);
        assert_eq!(d.confidence, confidence::EXTENSION);
    }

    #[test]
    fn a_readme_is_not_mistaken_for_yaml() {
        // The regression that motivates the whole YAML decision: a Markdown file documenting
        // options looks exactly like YAML to a naive sniffer.
        let md = "# Options\n\nname: what to call it\nversion: which one to use\n";
        assert_eq!(detect(&named("README.md", md)).format, FormatId::Markdown);
        // And on stdin, with no extension at all, the heading still settles it.
        assert_eq!(detect(&from_stdin(md)).format, FormatId::Markdown);
    }

    #[test]
    fn magic_bytes_outrank_the_extension() {
        // A PDF named `.txt` is still a PDF, and rendering it as text would be useless.
        let d = detect(&named("report.txt", "%PDF-1.7\nnot text"));
        assert_eq!(d.format, FormatId::Pdf);
        assert_eq!(d.confidence, confidence::MAGIC);
    }

    // ---------------- stdin: the route with no filename

    #[test]
    fn detects_every_data_format_on_stdin() {
        let cases: &[(&str, FormatId)] = &[
            (r#"{"a": [1, 2]}"#, FormatId::Json),
            ("<?xml version=\"1.0\"?><r><a/></r>", FormatId::Xml),
            ("<!DOCTYPE html><html></html>", FormatId::Html),
            ("[pkg]\nname = \"x\"\n", FormatId::Toml),
            ("a,b,c\n1,2,3\n4,5,6\n7,8,9\n", FormatId::Csv),
            ("---\nname: x\n", FormatId::Yaml),
            ("# Title\n\ntext\n", FormatId::Markdown),
        ];
        for (content, expected) in cases {
            let d = detect(&from_stdin(content));
            assert_eq!(d.format, *expected, "for {content:?} -> {}", d.reason);
        }
    }

    #[test]
    fn prose_on_stdin_falls_back_to_plain_text() {
        let d = detect(&from_stdin("Just some ordinary prose.\nWith two lines.\n"));
        assert_eq!(d.format, FormatId::PlainText);
        assert_eq!(d.confidence, confidence::FALLBACK);
    }

    #[test]
    fn a_binary_on_stdin_never_reaches_the_terminal_as_text() {
        let d = detect(&Source::from_bytes("<stdin>", vec![0x00, 0x01, 0x02, 0xFF]));
        assert_eq!(d.format, FormatId::Binary);
    }

    // ---------------- encoding

    #[test]
    fn prepare_configures_the_detected_encoding() {
        let mut src = Source::from_bytes(
            "t",
            b"Comit\xE9 de direcci\xF3n, se\xF1or, ni\xF1o".to_vec(),
        );
        let d = prepare(&registry(), &mut src, None).expect("should prepare");
        assert_ne!(
            src.encoding_name(),
            "UTF-8",
            "latin-1 must have been applied"
        );
        assert!(d.encoding.is_some());
        // And the text now decodes without replacement characters.
        assert!(!src.as_str().0.contains('\u{FFFD}'));
    }

    #[test]
    fn prepare_leaves_utf8_alone() {
        let mut src = Source::from_bytes("t", "acentos en español");
        prepare(&registry(), &mut src, None).unwrap();
        assert_eq!(src.encoding_name(), "UTF-8");
        assert_eq!(src.as_str().0, "acentos en español");
    }

    #[test]
    fn a_forced_encoding_overrides_detection() {
        let mut src = Source::from_bytes("t", b"caf\xE9".to_vec());
        prepare(&registry(), &mut src, Some("windows-1252")).unwrap();
        assert_eq!(src.encoding_name(), "windows-1252");
        assert_eq!(src.as_str().0, "café");
    }

    #[test]
    fn a_bad_forced_encoding_is_an_error() {
        let mut src = Source::from_bytes("t", "x");
        assert!(prepare(&registry(), &mut src, Some("nope")).is_err());
    }

    #[test]
    fn utf32_is_refused_with_a_way_out() {
        // encoding_rs implements no UTF-32, so the honest answer is to say so and suggest
        // iconv rather than decode it as something else and emit garbage.
        let mut bytes = vec![0xFF, 0xFE, 0x00, 0x00];
        bytes.extend_from_slice(&[b'a', 0, 0, 0]);
        let mut src = Source::from_bytes("t", bytes);
        let err = prepare(&registry(), &mut src, None).unwrap_err();
        assert!(err.to_string().contains("iconv"), "{err}");
    }

    // ---------------- --explain

    #[test]
    fn detect_all_shows_the_losing_layers_too() {
        // Seeing what was rejected is half of what makes --explain useful.
        let src = named("data.txt", r#"{"a": 1}"#);
        let all = registry().detect_all(&src);
        assert!(all.len() >= 2, "expected several opinions: {all:?}");
        assert!(all[0].confidence >= all[1].confidence, "sorted descending");
        assert!(all.iter().any(|d| d.format == FormatId::Json));
        assert!(all.iter().any(|d| d.format == FormatId::PlainText));
    }

    #[test]
    fn every_detection_explains_itself() {
        // A reason is not decoration: it is the only way to debug a wrong guess without
        // instrumenting the binary.
        for src in [
            from_stdin(r#"{"a":1}"#),
            from_stdin("# Title\n"),
            from_stdin("a,b\n1,2\n3,4\n5,6\n"),
            named("x.rs", "fn main() {}"),
            Source::from_bytes("t", b"%PDF-1.7".to_vec()),
        ] {
            let d = detect(&src);
            assert!(!d.reason.trim().is_empty(), "{} gave no reason", d.format);
        }
    }

    // ---------------- robustness

    #[test]
    fn nothing_panics_on_hostile_input() {
        let cases: Vec<Vec<u8>> = vec![
            vec![],
            b"PK\x03\x04".to_vec(),
            vec![0xFF; 100],
            b"{".to_vec(),
            b"<".to_vec(),
            "日本語".as_bytes().to_vec(),
            vec![0xEF, 0xBB, 0xBF],
            b"#!".to_vec(),
            b"---".to_vec(),
            vec![b'|'; 1000],
        ];
        for bytes in cases {
            let src = Source::from_bytes("t", bytes.clone());
            let _ = registry().detect_all(&src);
            let mut m = Source::from_bytes("t", bytes);
            let _ = prepare(&registry(), &mut m, None);
        }
    }
}
