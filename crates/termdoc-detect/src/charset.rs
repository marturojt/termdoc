//! Encoding detection: BOM, then statistics.
//!
//! Order matters and it is not arbitrary. A BOM is a *declaration* by whoever wrote the file,
//! so it wins outright. Absent one, valid UTF-8 is overwhelmingly the right answer for any
//! modern file — and importantly, long invalid-UTF-8 runs are statistically unlikely, so
//! "does it parse as UTF-8?" is itself a strong signal. Only when that fails does
//! `chardetng`'s guesswork come into play.

use termdoc_core::Source;

/// How many bytes to feed the statistical detector.
///
/// `chardetng` gets better with more input, but reading 64 KiB of a 2 GB log to guess an
/// encoding is a poor trade. This much is plenty for a decision.
const SNIFF_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharsetEvidence {
    /// A byte-order mark: the file declares its own encoding.
    Bom,
    /// It parses as valid UTF-8, which by itself is strong evidence.
    ValidUtf8,
    /// A statistical guess from `chardetng`.
    Statistical,
    /// Nothing conclusive; UTF-8 assumed.
    Assumed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Charset {
    /// A label `encoding_rs` (and therefore `Source::set_encoding`) accepts.
    pub label: &'static str,
    pub evidence: CharsetEvidence,
    /// Length in bytes of the BOM to skip, if there was one.
    pub bom_len: usize,
}

impl Charset {
    pub fn reason(&self) -> String {
        match self.evidence {
            CharsetEvidence::Bom => format!("{} declared by a BOM", self.label),
            CharsetEvidence::ValidUtf8 => "valid UTF-8".to_string(),
            CharsetEvidence::Statistical => {
                format!("{} guessed from byte statistics", self.label)
            }
            CharsetEvidence::Assumed => "UTF-8 assumed, nothing conclusive".to_string(),
        }
    }
}

const UTF8: Charset = Charset {
    label: "utf-8",
    evidence: CharsetEvidence::ValidUtf8,
    bom_len: 0,
};

pub fn detect(src: &Source) -> Charset {
    detect_bytes(src.peek(SNIFF_BYTES))
}

pub fn detect_bytes(bytes: &[u8]) -> Charset {
    if let Some(c) = from_bom(bytes) {
        return c;
    }

    if std::str::from_utf8(bytes).is_ok() {
        return UTF8;
    }

    // The sniff prefix may cut a multi-byte character in half, which would make valid UTF-8
    // look invalid. Retrying on the valid part avoids sending a perfectly good UTF-8 file to
    // the statistical detector because of where the prefix happened to end.
    if let Err(e) = std::str::from_utf8(bytes) {
        let cut_at_boundary = e.error_len().is_none();
        if cut_at_boundary && e.valid_up_to() > 0 {
            return UTF8;
        }
    }

    // Genuinely not UTF-8: hand it to the statistical detector.
    //
    // `Iso2022JpDetection::Deny` because ISO-2022-JP is an escape-based encoding whose
    // detection has false positives on binary-ish input, and it is vanishingly rare outside
    // Japanese email. `Utf8Detection::Deny` because we already established above that the
    // bytes are not valid UTF-8, so letting the detector answer "utf-8" could only be wrong.
    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    detector.feed(bytes, true);
    let encoding = detector.guess(None, chardetng::Utf8Detection::Deny);

    Charset {
        label: encoding.name(),
        evidence: CharsetEvidence::Statistical,
        bom_len: 0,
    }
}

fn from_bom(bytes: &[u8]) -> Option<Charset> {
    // UTF-8's BOM is checked before the UTF-16 ones because they share no prefix, but the
    // order still documents which is most common.
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Some(Charset {
            label: "utf-8",
            evidence: CharsetEvidence::Bom,
            bom_len: 3,
        });
    }
    // UTF-32 must be tested before UTF-16, since `FF FE 00 00` starts with the UTF-16LE BOM.
    if bytes.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) || bytes.starts_with(&[0x00, 0x00, 0xFE, 0xFF])
    {
        // `encoding_rs` implements no UTF-32 (the WHATWG spec dropped it), so the honest
        // answer is to report it and let the caller decide rather than mislabel the file.
        return Some(Charset {
            label: "utf-32",
            evidence: CharsetEvidence::Bom,
            bom_len: 4,
        });
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return Some(Charset {
            label: "utf-16le",
            evidence: CharsetEvidence::Bom,
            bom_len: 2,
        });
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return Some(Charset {
            label: "utf-16be",
            evidence: CharsetEvidence::Bom,
            bom_len: 2,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_ascii_is_utf8() {
        let c = detect_bytes(b"hello world");
        assert_eq!(c.label, "utf-8");
        assert_eq!(c.evidence, CharsetEvidence::ValidUtf8);
        assert_eq!(c.bom_len, 0);
    }

    #[test]
    fn a_utf8_bom_is_recognized_and_measured() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"hello");
        let c = detect_bytes(&bytes);
        assert_eq!(c.label, "utf-8");
        assert_eq!(c.evidence, CharsetEvidence::Bom);
        assert_eq!(c.bom_len, 3, "the BOM has to be skipped when rendering");
    }

    #[test]
    fn utf16_boms_are_recognized() {
        assert_eq!(detect_bytes(&[0xFF, 0xFE, b'a', 0]).label, "utf-16le");
        assert_eq!(detect_bytes(&[0xFE, 0xFF, 0, b'a']).label, "utf-16be");
    }

    #[test]
    fn utf32_is_tested_before_utf16() {
        // `FF FE 00 00` starts with the UTF-16LE BOM, so checking in the wrong order would
        // mislabel every UTF-32LE file.
        let c = detect_bytes(&[0xFF, 0xFE, 0x00, 0x00, b'a']);
        assert_eq!(c.label, "utf-32");
        assert_eq!(c.bom_len, 4);
    }

    #[test]
    fn a_bom_beats_the_content() {
        // The declaration wins even though the rest is valid UTF-8: whoever wrote the BOM
        // knew something we do not.
        let mut bytes = vec![0xFE, 0xFF];
        bytes.extend_from_slice(b"this is all ascii");
        assert_eq!(detect_bytes(&bytes).evidence, CharsetEvidence::Bom);
    }

    #[test]
    fn latin1_text_is_guessed_statistically() {
        // "Comité de dirección française" in latin-1: invalid UTF-8, and frequent enough
        // accented bytes for the detector to have something to work with.
        let latin1: Vec<u8> =
            b"Comit\xE9 de direcci\xF3n fran\xE7aise, se\xF1or, a\xF1o, ni\xF1o".to_vec();
        let c = detect_bytes(&latin1);
        assert_eq!(c.evidence, CharsetEvidence::Statistical);
        assert!(
            encoding_rs::Encoding::for_label(c.label.as_bytes()).is_some(),
            "the label '{}' must be usable with set_encoding",
            c.label
        );
    }

    #[test]
    fn valid_utf8_is_never_sent_to_the_guesser() {
        // Accented UTF-8 must not be re-guessed: the statistical detector could well decide
        // it is windows-1252 and turn "é" into "Ã©".
        let c = detect_bytes("Comité de dirección française".as_bytes());
        assert_eq!(c.evidence, CharsetEvidence::ValidUtf8);
        assert_eq!(c.label, "utf-8");
    }

    #[test]
    fn a_prefix_cut_mid_character_is_still_utf8() {
        // The regression this protects against: the sniff window ends in the middle of a
        // multi-byte character, and a whole UTF-8 file gets misdetected because of it.
        let mut bytes = "hello ".as_bytes().to_vec();
        bytes.extend_from_slice(&[0xE2, 0x82]); // an incomplete "€"
        let c = detect_bytes(&bytes);
        assert_eq!(
            c.evidence,
            CharsetEvidence::ValidUtf8,
            "a boundary cut is not an encoding error"
        );
    }

    #[test]
    fn every_label_returned_is_usable() {
        // The contract with `Source::set_encoding`: a label it cannot resolve would be a
        // runtime error at the worst possible moment.
        let samples: Vec<Vec<u8>> = vec![
            b"plain ascii".to_vec(),
            "acentos español".as_bytes().to_vec(),
            b"\xE9\xE9\xE9 latin".to_vec(),
            vec![0xEF, 0xBB, 0xBF, b'a'],
            vec![0xFF, 0xFE, b'a', 0],
        ];
        for s in samples {
            let c = detect_bytes(&s);
            if c.label == "utf-32" {
                // Documented and deliberate: encoding_rs implements no UTF-32.
                continue;
            }
            assert!(
                encoding_rs::Encoding::for_label(c.label.as_bytes()).is_some(),
                "unusable label: {}",
                c.label
            );
        }
    }

    #[test]
    fn the_empty_source_assumes_utf8() {
        let c = detect_bytes(b"");
        assert_eq!(c.label, "utf-8");
    }
}
