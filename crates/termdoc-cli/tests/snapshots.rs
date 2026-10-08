//! Output snapshots across (document x width x fidelity rung).
//!
//! These are the backbone of the project's test suite: they turn the degradation ladder from
//! docs/DESIGN.md §5 into something **verifiable** rather than a promise. A change in the
//! wrapping, the glyphs or the colors shows up here as a concrete diff that has to be approved
//! by hand.
//!
//! The pipeline is assembled explicitly rather than by invoking the binary, so the snapshots do
//! not depend on the terminal, the locale, or the environment of whoever runs them.
//!
//! To review and accept changes: `cargo insta review`.

use std::path::PathBuf;

use termdoc_core::{ReadContext, Registry, Source};
use termdoc_layout::{Layout, LayoutOptions, Theme};
use termdoc_term::{ColorDepth, Fidelity, GraphicsProto, UnicodeLevel};

fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../corpus")
        .join(name)
}

/// The rungs being pinned down, from highest to lowest.
fn rungs() -> Vec<(&'static str, Fidelity)> {
    vec![
        (
            "truecolor",
            Fidelity {
                color: ColorDepth::TrueColor,
                unicode: UnicodeLevel::Full,
                graphics: GraphicsProto::None,
                hyperlinks: true,
            },
        ),
        (
            "ansi16",
            Fidelity {
                color: ColorDepth::Ansi16,
                unicode: UnicodeLevel::Full,
                graphics: GraphicsProto::None,
                hyperlinks: false,
            },
        ),
        // The rung a pipe receives.
        ("plain", Fidelity::PLAIN),
        // Unicode available but no color: checks that the hierarchy survives.
        (
            "no-color-unicode",
            Fidelity {
                color: ColorDepth::None,
                unicode: UnicodeLevel::Full,
                graphics: GraphicsProto::None,
                hyperlinks: false,
            },
        ),
    ]
}

fn render(path: &PathBuf, width: usize, fidelity: Fidelity) -> String {
    let src = Source::open(path).expect("the corpus file must exist");

    let mut registry = Registry::new();
    // Both halves are needed: detection names the format, the readers know how to read it.
    termdoc_detect::register(&mut registry);
    termdoc_read_text::register(&mut registry);
    termdoc_read_data::register(&mut registry);
    let format = registry.detect_or_fallback(&src).format;
    let reader = registry.reader_for(format).expect("a reader is available");

    let events = reader
        .read(&src, &ReadContext::default())
        .expect("a successful read");

    let opts = LayoutOptions {
        width,
        fidelity,
        theme: if fidelity.color == ColorDepth::None {
            Theme::plain()
        } else {
            Theme::default()
        },
        line_numbers: false,
        inline_diagnostics: true,
    };

    let mut out: Vec<u8> = Vec::new();
    let mut backend = termdoc_backend::for_fidelity(fidelity);
    backend.begin(&mut out).expect("begin");
    for line in Layout::new(events, opts) {
        backend
            .write_line(&line.expect("a valid line"), &mut out)
            .expect("write");
    }
    backend.finish(&mut out).expect("finish");

    // Escapes are made visible so the snapshot is readable in a diff: a snapshot full of raw
    // control bytes is impossible to review.
    String::from_utf8(out)
        .expect("UTF-8 output")
        .replace('\x1b', "<ESC>")
}

macro_rules! snapshot_matrix {
    ($name:ident, $file:expr, $widths:expr) => {
        #[test]
        fn $name() {
            let path = corpus($file);
            for width in $widths {
                for (label, fidelity) in rungs() {
                    let output = render(&path, width, fidelity);
                    insta::assert_snapshot!(
                        format!("{}_{}_{}", stringify!($name), width, label),
                        output
                    );
                }
            }
        }
    };
}

snapshot_matrix!(basic, "basic.md", [40usize, 80]);
snapshot_matrix!(tables, "tables.md", [30usize, 60, 100]);
snapshot_matrix!(unicode, "unicode.md", [20usize, 40, 80]);
snapshot_matrix!(plain_text, "plain.txt", [40usize]);
// Minified on purpose: what the JSON reader buys is structure, and a snapshot of
// already-indented input would not show it.
snapshot_matrix!(data_json, "data.json", [40usize, 80]);
// Comments, anchors and a block scalar are in there on purpose: they are what a YAML parser's
// events would have lost.
snapshot_matrix!(data_yaml, "data.yaml", [40usize, 80]);
// Table headers, a multi-line array and a multi-line string: the shapes that need state
// carried from one line to the next.
snapshot_matrix!(data_toml, "data.toml", [40usize, 80]);
// A declaration, a doctype, a multi-line comment, CDATA and an attribute containing `>`:
// the shapes that make a regex highlighter wrong.
snapshot_matrix!(data_xml, "data.xml", [40usize, 80]);

/// Degrading changes the appearance, never the content.
///
/// The width is fixed at 100 and only the rung varies, isolating exactly what is being
/// measured. Comparing the rungs against each other would produce false positives, because they
/// legitimately differ: at full fidelity a link uses OSC 8 and its target travels inside the
/// escape sequence, while without OSC 8 it shows up as `[1]` plus a reference list. So the
/// check is against an explicit list of content instead.
#[test]
fn no_rung_loses_content() {
    let cases: &[(&str, &[&str])] = &[
        (
            "basic.md",
            &[
                "termdoc",
                "universal",
                "Features",
                "Extensible",
                "Markdown",
                "pending",
                "println",
                "GFM alert",
            ],
        ),
        (
            "tables.md",
            &["Column with a long name", "first", "third", "filler"],
        ),
        ("unicode.md", &["Cell widths", "Combining", "español"]),
        ("plain.txt", &["plain text with no markers", "tabbed"]),
    ];

    for (file, expected) in cases {
        let path = corpus(file);
        for (label, fidelity) in rungs() {
            let visible = strip_escapes(&render(&path, 100, fidelity));
            // Reflowing may break a phrase across two lines; whitespace is normalized so that
            // does not count as loss.
            let flat: String = visible.split_whitespace().collect::<Vec<_>>().join(" ");
            for fragment in *expected {
                let needle: String = fragment.split_whitespace().collect::<Vec<_>>().join(" ");
                assert!(
                    flat.contains(&needle),
                    "'{fragment}' vanished from {file} at the {label} rung"
                );
            }
        }
    }
}

/// Narrowing the width loses not a single character of content.
///
/// This cannot be checked by searching for words: in a narrow table the engine splits words
/// inside the cell (phase 2 of width allocation) and, on top of that, the grid interleaves the
/// columns line by line, so "Markdown" comes out as `Mark`/`down` on two lines **with the other
/// columns' content in between**. That is correct, but it defeats any substring search.
///
/// What *is* invariant is the **count of content characters**: with escapes, borders and spaces
/// removed, a document must have exactly the same characters at 20 columns as at 100.
#[test]
fn no_width_loses_characters() {
    for file in ["basic.md", "tables.md", "unicode.md", "plain.txt"] {
        let path = corpus(file);
        for (label, fidelity) in rungs() {
            let reference = fingerprint(&render(&path, 100, fidelity));
            for width in [20usize, 30, 40, 60, 80] {
                let got = fingerprint(&render(&path, width, fidelity));
                assert_eq!(
                    got, reference,
                    "{file} loses or gains characters going from 100 to {width} columns at the \
                     {label} rung"
                );
            }
        }
    }
}

/// Border and grid-padding characters.
const DECORATION: &str = "│┌┐└┘├┤┬┴┼─|+-=";

/// The sorted multiset of content characters.
///
/// Decoration is removed in two steps, and the order matters:
///
/// 1. Lines that are **only** decoration (horizontal rules, table borders) are dropped. Their
///    length depends on the width, so counting them would fail the test without anything having
///    been lost.
/// 2. From the remaining lines, only the vertical cell separators are stripped.
///
/// Doing it this way, rather than filtering `-` across the whole text, keeps the test sensitive
/// to hyphens that really are content.
fn fingerprint(s: &str) -> Vec<char> {
    let visible = strip_escapes(s);
    let mut chars: Vec<char> = visible
        .lines()
        .filter(|line| {
            let significant: Vec<char> = line.chars().filter(|c| !c.is_whitespace()).collect();
            // The line is kept unless everything it holds is decoration.
            significant.is_empty() || !significant.iter().all(|c| DECORATION.contains(*c))
        })
        .flat_map(|line| line.chars())
        .filter(|c| !c.is_whitespace() && !"│|".contains(*c))
        .collect();
    chars.sort_unstable();
    chars
}

fn strip_escapes(s: &str) -> String {
    // Escapes arrive already marked as <ESC>; the whole sequence is removed.
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("<ESC>") {
        out.push_str(&rest[..i]);
        rest = &rest[i + "<ESC>".len()..];
        if let Some(stripped) = rest.strip_prefix('[') {
            let end = stripped
                .find(|c: char| c.is_ascii_alphabetic())
                .map(|p| p + 2)
                .unwrap_or(rest.len());
            rest = &rest[end.min(rest.len())..];
        } else if let Some(stripped) = rest.strip_prefix(']') {
            let end = stripped.find('\\').map(|p| p + 2).unwrap_or(stripped.len());
            rest = &rest[end.min(rest.len())..];
        }
    }
    out.push_str(rest);
    out
}
