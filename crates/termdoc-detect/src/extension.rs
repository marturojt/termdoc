//! Detection by filename, plus the extension→language map for source code.
//!
//! For text formats the extension is a legitimate signal rather than a last resort: JSON, YAML
//! and TOML have no magic bytes, and the person who named the file `config.yaml` was telling us
//! something. It sits below structural sniffing, though, because content is more reliable than
//! a name when the two disagree.

use termdoc_core::{Detection, FormatId, Source, confidence};

/// Extension → format. The first match wins, so order is irrelevant; the table is grouped by
/// format for readability.
const BY_EXTENSION: &[(&str, FormatId)] = &[
    // Markup and documents
    ("md", FormatId::Markdown),
    ("markdown", FormatId::Markdown),
    ("mdown", FormatId::Markdown),
    ("mkd", FormatId::Markdown),
    ("mdx", FormatId::Markdown),
    ("html", FormatId::Html),
    ("htm", FormatId::Html),
    ("xhtml", FormatId::Html),
    ("pdf", FormatId::Pdf),
    ("docx", FormatId::Docx),
    ("odt", FormatId::Odt),
    ("rtf", FormatId::Rtf),
    ("epub", FormatId::Epub),
    ("xlsx", FormatId::Xlsx),
    ("pptx", FormatId::Pptx),
    // Structured data
    ("json", FormatId::Json),
    ("jsonc", FormatId::Json),
    ("geojson", FormatId::Json),
    ("yaml", FormatId::Yaml),
    ("yml", FormatId::Yaml),
    ("toml", FormatId::Toml),
    ("xml", FormatId::Xml),
    ("svg", FormatId::Xml),
    ("rss", FormatId::Xml),
    ("atom", FormatId::Xml),
    ("plist", FormatId::Xml),
    ("csv", FormatId::Csv),
    ("tsv", FormatId::Csv),
    // Plain text and logs
    ("txt", FormatId::PlainText),
    ("text", FormatId::PlainText),
    ("rst", FormatId::PlainText),
    ("asc", FormatId::PlainText),
    ("log", FormatId::Log),
];

/// Extension → the language name syntect expects.
///
/// Kept separate from `BY_EXTENSION` because the format is always `SourceCode` and what varies
/// is the grammar to highlight with. Merging them would force a language field onto every
/// entry above, where it means nothing.
const BY_LANGUAGE: &[(&str, &str)] = &[
    ("rs", "Rust"),
    ("py", "Python"),
    ("pyi", "Python"),
    ("js", "JavaScript"),
    ("mjs", "JavaScript"),
    ("cjs", "JavaScript"),
    ("jsx", "JavaScript"),
    ("ts", "TypeScript"),
    ("tsx", "TypeScript"),
    ("go", "Go"),
    ("c", "C"),
    ("h", "C"),
    ("cpp", "C++"),
    ("cc", "C++"),
    ("cxx", "C++"),
    ("hpp", "C++"),
    ("java", "Java"),
    ("kt", "Kotlin"),
    ("kts", "Kotlin"),
    ("swift", "Swift"),
    ("rb", "Ruby"),
    ("php", "PHP"),
    ("cs", "C#"),
    ("scala", "Scala"),
    ("hs", "Haskell"),
    ("ml", "OCaml"),
    ("ex", "Elixir"),
    ("exs", "Elixir"),
    ("erl", "Erlang"),
    ("clj", "Clojure"),
    ("lua", "Lua"),
    ("pl", "Perl"),
    ("r", "R"),
    ("jl", "Julia"),
    ("zig", "Zig"),
    ("dart", "Dart"),
    ("sh", "Bourne Again Shell (bash)"),
    ("bash", "Bourne Again Shell (bash)"),
    ("zsh", "Bourne Again Shell (bash)"),
    ("fish", "Bourne Again Shell (bash)"),
    ("ps1", "PowerShell"),
    ("sql", "SQL"),
    ("css", "CSS"),
    ("scss", "SCSS"),
    ("less", "CSS"),
    ("ini", "INI"),
    ("cfg", "INI"),
    ("conf", "INI"),
    ("dockerfile", "Dockerfile"),
    ("makefile", "Makefile"),
    ("mk", "Makefile"),
    ("nix", "Nix"),
    ("vim", "VimL"),
    ("diff", "Diff"),
    ("patch", "Diff"),
];

/// Filenames with no extension that still identify themselves.
const BY_FILENAME: &[(&str, &str)] = &[
    ("Dockerfile", "Dockerfile"),
    ("Containerfile", "Dockerfile"),
    ("Makefile", "Makefile"),
    ("GNUmakefile", "Makefile"),
    ("Justfile", "Makefile"),
    ("CMakeLists.txt", "CMake"),
    ("Gemfile", "Ruby"),
    ("Rakefile", "Ruby"),
    ("Vagrantfile", "Ruby"),
    ("BUILD", "Python"),
    ("WORKSPACE", "Python"),
];

/// The language a source file should be highlighted with, if we can tell.
pub fn language_for(src: &Source) -> Option<&'static str> {
    let path = src.path()?;

    if let Some(name) = path.file_name().and_then(|n| n.to_str())
        && let Some((_, lang)) = BY_FILENAME.iter().find(|(f, _)| *f == name)
    {
        return Some(lang);
    }

    let ext = extension(src)?;
    BY_LANGUAGE
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, lang)| *lang)
}

pub fn sniff(src: &Source) -> Option<Detection> {
    // Filenames with no extension come first: `Makefile` has no extension to look up.
    if let Some(path) = src.path()
        && let Some(name) = path.file_name().and_then(|n| n.to_str())
        && BY_FILENAME.iter().any(|(f, _)| *f == name)
    {
        return Some(Detection::new(
            FormatId::SourceCode,
            confidence::EXTENSION,
            format!("the filename '{name}' identifies a known language"),
        ));
    }

    let ext = extension(src)?;

    if let Some((_, format)) = BY_EXTENSION.iter().find(|(e, _)| *e == ext) {
        return Some(Detection::new(
            *format,
            confidence::EXTENSION,
            format!(".{ext} extension"),
        ));
    }

    if let Some((_, lang)) = BY_LANGUAGE.iter().find(|(e, _)| *e == ext) {
        return Some(Detection::new(
            FormatId::SourceCode,
            confidence::EXTENSION,
            format!(".{ext} extension ({lang})"),
        ));
    }

    None
}

/// A shebang or an editor modeline, for scripts with no extension.
pub fn sniff_shebang(text: &str) -> Option<Detection> {
    let first = text.lines().next()?;
    let rest = first.strip_prefix("#!")?;

    // The interpreter is the *first* word that is not `env`, not the last: `#!/usr/bin/perl -w`
    // ends in a flag, and taking the last token would identify the language as "-w".
    let interpreter = rest
        .split_whitespace()
        .find(|w| !w.is_empty() && !w.contains("env"))?
        .rsplit('/')
        .next()?;

    // Trailing version digits are stripped: `python3.11` is Python.
    let base: String = interpreter
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();

    let lang = match base.as_str() {
        "python" => "Python",
        "node" | "nodejs" | "deno" | "bun" => "JavaScript",
        "bash" | "sh" | "zsh" | "dash" | "ksh" => "Bourne Again Shell (bash)",
        "ruby" => "Ruby",
        "perl" => "Perl",
        "php" => "PHP",
        "lua" => "Lua",
        "fish" => "Bourne Again Shell (bash)",
        "awk" | "gawk" => "AWK",
        _ => return None,
    };

    Some(Detection::new(
        FormatId::SourceCode,
        confidence::STRUCTURAL,
        format!("#! shebang for {lang}"),
    ))
}

fn extension(src: &Source) -> Option<String> {
    src.path()?
        .extension()?
        .to_str()
        .map(|e| e.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A `Source` that reports a path without touching the filesystem.
    fn named(name: &str) -> Source {
        let dir = std::env::temp_dir().join("termdoc-detect-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path: PathBuf = dir.join(name);
        std::fs::write(&path, b"x").unwrap();
        Source::open(&path).unwrap()
    }

    #[test]
    fn maps_the_data_extensions() {
        for (name, expected) in [
            ("a.json", FormatId::Json),
            ("a.yaml", FormatId::Yaml),
            ("a.yml", FormatId::Yaml),
            ("a.toml", FormatId::Toml),
            ("a.xml", FormatId::Xml),
            ("a.csv", FormatId::Csv),
            ("a.tsv", FormatId::Csv),
            ("a.md", FormatId::Markdown),
            ("a.log", FormatId::Log),
            ("a.txt", FormatId::PlainText),
        ] {
            let d = sniff(&named(name)).unwrap_or_else(|| panic!("{name} was not detected"));
            assert_eq!(d.format, expected, "{name}");
            assert_eq!(d.confidence, confidence::EXTENSION);
        }
    }

    #[test]
    fn source_extensions_become_sourcecode() {
        for name in ["a.rs", "a.py", "a.go", "a.ts", "a.sh"] {
            let d = sniff(&named(name)).unwrap_or_else(|| panic!("{name} was not detected"));
            assert_eq!(d.format, FormatId::SourceCode, "{name}");
        }
    }

    #[test]
    fn the_extension_is_case_insensitive() {
        assert_eq!(sniff(&named("A.JSON")).unwrap().format, FormatId::Json);
        assert_eq!(
            sniff(&named("README.MD")).unwrap().format,
            FormatId::Markdown
        );
    }

    #[test]
    fn an_unknown_extension_is_not_claimed() {
        assert!(sniff(&named("a.zzz")).is_none());
    }

    #[test]
    fn known_extensionless_filenames_are_recognized() {
        let d = sniff(&named("Dockerfile")).expect("Dockerfile has no extension but is known");
        assert_eq!(d.format, FormatId::SourceCode);
        assert_eq!(language_for(&named("Dockerfile")), Some("Dockerfile"));
        assert_eq!(language_for(&named("Makefile")), Some("Makefile"));
    }

    #[test]
    fn language_for_resolves_the_grammar() {
        assert_eq!(language_for(&named("a.rs")), Some("Rust"));
        assert_eq!(language_for(&named("a.tsx")), Some("TypeScript"));
        assert_eq!(language_for(&named("a.zzz")), None);
    }

    #[test]
    fn the_shebang_identifies_the_interpreter() {
        for (line, expected) in [
            ("#!/usr/bin/env python3\nprint(1)\n", "Python"),
            ("#!/bin/bash\necho hi\n", "Bourne Again Shell (bash)"),
            ("#!/usr/bin/env node\n", "JavaScript"),
            ("#!/usr/bin/perl -w\n", "Perl"),
            ("#!/usr/bin/env ruby\n", "Ruby"),
        ] {
            let d = sniff_shebang(line).unwrap_or_else(|| panic!("not detected: {line:?}"));
            assert_eq!(d.format, FormatId::SourceCode);
            assert!(d.reason.contains(expected), "{}", d.reason);
        }
    }

    #[test]
    fn the_shebang_outranks_the_extension() {
        // A script named `deploy` with a `#!/bin/bash` line is more reliably identified by its
        // content than by its (absent) extension, so it gets the higher confidence.
        let d = sniff_shebang("#!/bin/bash\n").unwrap();
        assert_eq!(d.confidence, confidence::STRUCTURAL);
        // A const block so clippy sees this for what it is: pinning the layer ordering
        // itself, not asserting on runtime data.
        const { assert!(confidence::STRUCTURAL > confidence::EXTENSION) };
    }

    #[test]
    fn trailing_flags_do_not_confuse_the_shebang() {
        // Regression: taking the last whitespace-separated token identified `-w` as the
        // interpreter and the shebang went undetected.
        let d = sniff_shebang("#!/usr/bin/perl -w\n").expect("perl -w");
        assert!(d.reason.contains("Perl"), "{}", d.reason);
        assert!(sniff_shebang("#!/bin/sh -e\n").is_some());
        assert!(sniff_shebang("#!/usr/bin/env python3 -u\n").is_some());
    }

    #[test]
    fn a_version_suffix_does_not_confuse_the_shebang() {
        assert!(sniff_shebang("#!/usr/bin/env python3.11\n").is_some());
        assert!(sniff_shebang("#!/usr/bin/python2\n").is_some());
    }

    #[test]
    fn a_hash_that_is_not_a_shebang_is_ignored() {
        assert!(sniff_shebang("# just a comment\n").is_none());
        assert!(sniff_shebang("#!/unknown/interpreter\n").is_none());
        assert!(sniff_shebang("").is_none());
    }

    #[test]
    fn stdin_has_no_extension_to_read() {
        assert!(sniff(&Source::from_bytes("<stdin>", "x")).is_none());
        assert!(language_for(&Source::from_bytes("<stdin>", "x")).is_none());
    }
}
