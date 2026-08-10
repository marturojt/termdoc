//! Format identity and detection results.

use std::borrow::Cow;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FormatId {
    PlainText,
    Markdown,
    Log,
    SourceCode,
    Json,
    Yaml,
    Toml,
    Xml,
    Csv,
    Html,
    Pdf,
    Docx,
    Odt,
    Rtf,
    Epub,
    Xlsx,
    Pptx,
    /// A format contributed by a plugin, identified by its name.
    External(&'static str),
    /// Not interpretable as text: shown as a hex dump.
    Binary,
}

impl FormatId {
    pub fn name(self) -> &'static str {
        match self {
            FormatId::PlainText => "text",
            FormatId::Markdown => "markdown",
            FormatId::Log => "log",
            FormatId::SourceCode => "code",
            FormatId::Json => "json",
            FormatId::Yaml => "yaml",
            FormatId::Toml => "toml",
            FormatId::Xml => "xml",
            FormatId::Csv => "csv",
            FormatId::Html => "html",
            FormatId::Pdf => "pdf",
            FormatId::Docx => "docx",
            FormatId::Odt => "odt",
            FormatId::Rtf => "rtf",
            FormatId::Epub => "epub",
            FormatId::Xlsx => "xlsx",
            FormatId::Pptx => "pptx",
            FormatId::External(n) => n,
            FormatId::Binary => "binary",
        }
    }

    /// Parses the value of `--from`.
    pub fn parse(s: &str) -> Option<Self> {
        let known = [
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
        ];
        let lower = s.to_ascii_lowercase();
        // Common aliases that people will type before the canonical name.
        let lower = match lower.as_str() {
            "md" => "markdown".to_string(),
            "txt" => "text".to_string(),
            "yml" => "yaml".to_string(),
            "htm" => "html".to_string(),
            "rs" | "py" | "js" | "source" => "code".to_string(),
            other => other.to_string(),
        };
        known.into_iter().find(|f| f.name() == lower)
    }

    pub fn all_names() -> Vec<&'static str> {
        vec![
            "text", "markdown", "log", "code", "json", "yaml", "toml", "xml", "csv", "html", "pdf",
            "docx", "odt", "rtf", "epub", "xlsx", "pptx", "binary",
        ]
    }
}

impl std::fmt::Display for FormatId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// The 0..=100 confidence with which a detector claims a format.
///
/// It exists so `--explain` can justify the choice, and so a plugin's detector cannot
/// hijack detection on a weak hunch.
pub type Confidence = u8;

pub mod confidence {
    use super::Confidence;
    /// The user said so with `--from`. Nothing outranks it.
    pub const EXPLICIT: Confidence = 100;
    /// Unambiguous magic bytes, or a characteristic entry inside a ZIP.
    pub const MAGIC: Confidence = 90;
    /// A structural sniff that actually parsed the prefix.
    pub const STRUCTURAL: Confidence = 70;
    /// The filename extension.
    pub const EXTENSION: Confidence = 50;
    /// A weak heuristic (marker frequency and the like).
    pub const HEURISTIC: Confidence = 30;
    /// Last resort.
    pub const FALLBACK: Confidence = 1;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detection {
    pub format: FormatId,
    pub confidence: Confidence,
    /// Why it was chosen. Printed by `--explain`; not decoration, but the only way to
    /// debug a wrong guess without instrumenting the binary.
    pub reason: Cow<'static, str>,
    /// Name of the detected encoding, when one was determined.
    pub encoding: Option<&'static str>,
}

impl Detection {
    pub fn new(
        format: FormatId,
        confidence: Confidence,
        reason: impl Into<Cow<'static, str>>,
    ) -> Self {
        Detection {
            format,
            confidence,
            reason: reason.into(),
            encoding: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_aliases() {
        assert_eq!(FormatId::parse("md"), Some(FormatId::Markdown));
        assert_eq!(FormatId::parse("MARKDOWN"), Some(FormatId::Markdown));
        assert_eq!(FormatId::parse("yml"), Some(FormatId::Yaml));
        assert_eq!(FormatId::parse("txt"), Some(FormatId::PlainText));
        assert_eq!(FormatId::parse("nonexistent"), None);
    }

    #[test]
    fn all_names_covers_every_parseable_format() {
        for name in FormatId::all_names() {
            assert!(
                FormatId::parse(name).is_some(),
                "'{name}' is advertised in --help but does not parse"
            );
        }
    }
}
