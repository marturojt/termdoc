//! Magic bytes, and the intra-ZIP disambiguation.
//!
//! The most reliable layer: a file starting with `%PDF-` is a PDF, full stop.
//!
//! It is also where the detail that matters most lives. DOCX, ODT, EPUB, XLSX and PPTX are all
//! ZIP archives, so they share the exact same magic bytes. Without looking *inside* the
//! archive, all five are indistinguishable and the layer is worthless for the entire Office
//! family.

use termdoc_core::{Detection, FormatId, Source, confidence};

/// The signatures we recognize ourselves, ahead of `infer`.
///
/// `infer` covers a long tail of media formats we do not care about, and its ZIP answer is just
/// "zip", so the formats that matter here are handled directly.
const SIGNATURES: &[(&[u8], FormatId, &str)] = &[
    (b"%PDF-", FormatId::Pdf, "%PDF- signature"),
    (b"{\\rtf", FormatId::Rtf, "{\\rtf signature"),
    // Legacy Microsoft Office (OLE2). Not supported, but recognizing it lets us say so
    // instead of dumping binary into the terminal.
    (
        b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1",
        FormatId::Binary,
        "OLE2 container (legacy .doc/.xls)",
    ),
];

const ZIP_SIGNATURES: &[&[u8]] = &[
    b"PK\x03\x04", // a normal local file header
    b"PK\x05\x06", // an empty archive
    b"PK\x07\x08", // spanned
];

pub fn sniff(src: &Source) -> Option<Detection> {
    let probe = src.probe();

    for (sig, format, reason) in SIGNATURES {
        if probe.starts_with(sig) {
            return Some(Detection::new(*format, confidence::MAGIC, *reason));
        }
    }

    if ZIP_SIGNATURES.iter().any(|s| probe.starts_with(s)) {
        return Some(disambiguate_zip(src));
    }

    if let Some(kind) = infer::get(probe) {
        // `infer` also recognizes textual formats — it answers `text/xml` for an XML
        // declaration — and claiming those as binary at confidence 90 would outrank the
        // structural layer that knows how to read them. Anything textual is handed onward;
        // only genuinely opaque media and archives stop here.
        if kind.mime_type().starts_with("text/") {
            return None;
        }
        return Some(Detection::new(
            FormatId::Binary,
            confidence::MAGIC,
            format!("{} ({})", kind.mime_type(), kind.extension()),
        ));
    }

    None
}

/// Tells the ZIP-based formats apart by looking at the archive's contents.
///
/// A full ZIP parse is deliberately avoided: reading the central directory means seeking to the
/// end of the file, which for a 500 MB EPUB pulls in pages we do not need just to name the
/// format. Instead the first entries' names are read straight from the local file headers at the
/// start of the archive — and for EPUB and ODT that is decisive, because the spec *requires*
/// `mimetype` to be the first entry.
fn disambiguate_zip(src: &Source) -> Detection {
    let probe = src.probe();

    // EPUB and ODT store an uncompressed `mimetype` entry first, so its value is sitting in
    // plain text within the first few hundred bytes.
    if let Some(mime) = first_entry_mimetype(probe) {
        let format = match mime {
            m if m.starts_with("application/epub") => Some(FormatId::Epub),
            "application/vnd.oasis.opendocument.text" => Some(FormatId::Odt),
            m if m.starts_with("application/vnd.oasis.opendocument") => Some(FormatId::Odt),
            _ => None,
        };
        if let Some(format) = format {
            return Detection::new(
                format,
                confidence::MAGIC,
                format!("ZIP with a '{mime}' mimetype entry"),
            );
        }
    }

    // OOXML has no `mimetype` entry, so a characteristic path is used instead. These names
    // appear in the local file headers near the start of the archive.
    for (needle, format, reason) in [
        (
            &b"word/document.xml"[..],
            FormatId::Docx,
            "ZIP containing word/document.xml",
        ),
        (
            &b"xl/workbook.xml"[..],
            FormatId::Xlsx,
            "ZIP containing xl/workbook.xml",
        ),
        (
            &b"ppt/presentation.xml"[..],
            FormatId::Pptx,
            "ZIP containing ppt/presentation.xml",
        ),
    ] {
        if find(probe, needle).is_some() {
            return Detection::new(format, confidence::MAGIC, reason);
        }
    }

    // A plain ZIP. Reported as binary because there is no reader for archives, and saying so
    // beats guessing.
    Detection::new(
        FormatId::Binary,
        confidence::MAGIC,
        "ZIP archive with no recognized document structure",
    )
}

/// Reads the `mimetype` value when it is the archive's first entry.
///
/// Layout of a local file header: `PK\x03\x04`, 22 bytes of fields, the filename length at
/// offset 26, the extra-field length at 28, then the name and the data.
fn first_entry_mimetype(probe: &[u8]) -> Option<&str> {
    if probe.len() < 30 || !probe.starts_with(b"PK\x03\x04") {
        return None;
    }

    let name_len = u16::from_le_bytes([probe[26], probe[27]]) as usize;
    let extra_len = u16::from_le_bytes([probe[28], probe[29]]) as usize;
    let name_start = 30usize;
    let name_end = name_start.checked_add(name_len)?;
    if name_end > probe.len() {
        return None;
    }

    if &probe[name_start..name_end] != b"mimetype" {
        return None;
    }

    // The value follows the extra field. It is stored uncompressed precisely so it can be read
    // like this.
    let data_start = name_end.checked_add(extra_len)?;
    if data_start >= probe.len() {
        return None;
    }

    // The declared size can be zero for streamed entries, so the value is read up to the next
    // `PK` header instead of trusting the length field.
    let rest = &probe[data_start..];
    let end = find(rest, b"PK").unwrap_or(rest.len()).min(128);
    std::str::from_utf8(&rest[..end]).ok().map(|s| s.trim())
}

/// A plain substring search. No `memchr` dependency for a scan over 8 KB.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(bytes: Vec<u8>) -> Source {
        Source::from_bytes("t", bytes)
    }

    /// Builds a minimal ZIP whose first entry is an uncompressed `mimetype`.
    fn zip_with_mimetype(mime: &str) -> Vec<u8> {
        let name = b"mimetype";
        let mut z = Vec::new();
        z.extend_from_slice(b"PK\x03\x04");
        z.extend_from_slice(&[0u8; 22]); // version, flags, method, times, crc, sizes
        z.extend_from_slice(&(name.len() as u16).to_le_bytes()); // offset 26
        z.extend_from_slice(&0u16.to_le_bytes()); // offset 28: no extra field
        z.extend_from_slice(name);
        z.extend_from_slice(mime.as_bytes());
        z.extend_from_slice(b"PK\x03\x04"); // the next entry's header
        z.extend_from_slice(&[0u8; 26]);
        z
    }

    #[test]
    fn recognizes_pdf() {
        let d = sniff(&src(b"%PDF-1.7\nrest".to_vec())).unwrap();
        assert_eq!(d.format, FormatId::Pdf);
        assert_eq!(d.confidence, confidence::MAGIC);
    }

    #[test]
    fn recognizes_rtf() {
        assert_eq!(
            sniff(&src(b"{\\rtf1\\ansi".to_vec())).unwrap().format,
            FormatId::Rtf
        );
    }

    #[test]
    fn epub_is_told_apart_from_other_zips() {
        // This is the whole point of the layer: without looking inside, this is just "a ZIP".
        let d = sniff(&src(zip_with_mimetype("application/epub+zip"))).unwrap();
        assert_eq!(d.format, FormatId::Epub);
        assert!(d.reason.contains("mimetype"), "{}", d.reason);
    }

    #[test]
    fn odt_is_told_apart_by_its_mimetype() {
        let d = sniff(&src(zip_with_mimetype(
            "application/vnd.oasis.opendocument.text",
        )))
        .unwrap();
        assert_eq!(d.format, FormatId::Odt);
    }

    #[test]
    fn ooxml_is_told_apart_by_a_characteristic_path() {
        // DOCX has no mimetype entry, so the entry name is what identifies it.
        let mut z = Vec::new();
        z.extend_from_slice(b"PK\x03\x04");
        z.extend_from_slice(&[0u8; 22]);
        z.extend_from_slice(&(b"[Content_Types].xml".len() as u16).to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(b"[Content_Types].xml");
        z.extend_from_slice(b"...some content...");
        z.extend_from_slice(b"PK\x03\x04");
        z.extend_from_slice(&[0u8; 22]);
        z.extend_from_slice(&(b"word/document.xml".len() as u16).to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(b"word/document.xml");

        let d = sniff(&src(z)).unwrap();
        assert_eq!(d.format, FormatId::Docx);
    }

    #[test]
    fn distinguishes_xlsx_from_pptx() {
        for (path, expected) in [
            ("xl/workbook.xml", FormatId::Xlsx),
            ("ppt/presentation.xml", FormatId::Pptx),
        ] {
            let mut z = b"PK\x03\x04".to_vec();
            z.extend_from_slice(&[0u8; 26]);
            z.extend_from_slice(path.as_bytes());
            assert_eq!(sniff(&src(z)).unwrap().format, expected, "{path}");
        }
    }

    #[test]
    fn a_plain_zip_is_reported_as_binary_not_guessed() {
        let mut z = b"PK\x03\x04".to_vec();
        z.extend_from_slice(&[0u8; 26]);
        z.extend_from_slice(b"random/file.txt");
        let d = sniff(&src(z)).unwrap();
        assert_eq!(d.format, FormatId::Binary);
        assert!(d.reason.contains("ZIP"), "{}", d.reason);
    }

    #[test]
    fn legacy_office_is_recognized_so_it_can_be_refused_clearly() {
        // Being able to say "this is a legacy .doc" beats dumping OLE2 bytes on the terminal.
        let d = sniff(&src(b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1extra".to_vec())).unwrap();
        assert_eq!(d.format, FormatId::Binary);
        assert!(d.reason.contains("OLE2"), "{}", d.reason);
    }

    #[test]
    fn images_are_binary() {
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec();
        let d = sniff(&src(png)).unwrap();
        assert_eq!(d.format, FormatId::Binary);
        assert!(d.reason.contains("image/png"), "{}", d.reason);
    }

    #[test]
    fn textual_formats_are_handed_on_not_claimed_as_binary() {
        // Regression: `infer` answers text/xml for an XML declaration, and claiming it as
        // binary at confidence 90 beat the structural layer that can actually read it.
        assert!(
            sniff(&src(br#"<?xml version="1.0"?><r/>"#.to_vec())).is_none(),
            "XML must reach the structural layer"
        );
    }

    #[test]
    fn plain_text_has_no_magic() {
        assert!(sniff(&src(b"just some text".to_vec())).is_none());
        assert!(sniff(&src(b"# a markdown heading".to_vec())).is_none());
    }

    #[test]
    fn a_truncated_zip_header_does_not_panic() {
        // The regression this guards: reading the filename length past the end of the buffer.
        for len in 0..40 {
            let mut z = b"PK\x03\x04".to_vec();
            z.extend_from_slice(&vec![0xFFu8; len]);
            let _ = sniff(&src(z));
        }
    }

    #[test]
    fn an_absurd_filename_length_does_not_panic() {
        // A malformed archive claiming a 65535-byte name in a 40-byte file.
        let mut z = b"PK\x03\x04".to_vec();
        z.extend_from_slice(&[0u8; 22]);
        z.extend_from_slice(&u16::MAX.to_le_bytes());
        z.extend_from_slice(&u16::MAX.to_le_bytes());
        z.extend_from_slice(b"short");
        let _ = sniff(&src(z));
    }

    #[test]
    fn the_empty_source_has_no_magic() {
        assert!(sniff(&src(Vec::new())).is_none());
    }
}
