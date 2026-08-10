//! Property tests for the layout engine's invariants.
//!
//! Example-based tests check the cases we thought of; these check the ones we did not. The
//! invariants are those documented in `wrap.rs`, and each has a concrete reason to be here:
//!
//! 1. **No line exceeds the width in display cells.** If this fails, the output overflows
//!    and the terminal breaks it wherever it likes, ruining tables and indentation.
//! 2. **A break never splits a grapheme cluster.** If this fails, broken characters appear:
//!    a stray accent, half an emoji.
//! 3. **No text is lost or invented.** That is what separates a viewer from an editor.

use std::borrow::Cow;

use proptest::prelude::*;
use termdoc_core::{Segment, Style};
use termdoc_layout::{WrapBuffer, display_width};
use unicode_segmentation::UnicodeSegmentation;

/// Text containing the material that breaks naive implementations: double-width
/// ideographs, combining marks, ZWJ emoji, spaces and long words.
fn varied_text() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just("a".to_string()),
            Just("word".to_string()),
            Just(" ".to_string()),
            Just("  ".to_string()),
            Just("\t".to_string()),
            // Double width.
            Just("日".to_string()),
            Just("本語".to_string()),
            // Combining: "e" plus an acute accent.
            Just("e\u{0301}".to_string()),
            // ZWJ emoji: a single cluster, many bytes.
            Just("\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}".to_string()),
            // Regional-indicator flag: two scalars, one cluster.
            Just("\u{1f1f2}\u{1f1fd}".to_string()),
            Just("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
        ],
        0..30,
    )
    .prop_map(|parts| parts.concat())
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut buf = WrapBuffer::new();
    buf.push_text(Cow::Borrowed(text), Style::PLAIN, None);
    buf.wrap(width, &[], None)
        .iter()
        .map(|l| l.to_plain_string())
        .collect()
}

proptest! {
    #[test]
    fn no_line_exceeds_the_width(text in varied_text(), width in 1usize..60) {
        // With the single exception documented in `wrap.rs`: an indivisible grapheme
        // cluster wider than the entire line. Splitting it would produce half an emoji,
        // which is worse than overflowing one cell in a one-column terminal.
        for line in wrap(&text, width) {
            let measured = display_width(&line);
            if measured <= width {
                continue;
            }
            let clusters = line.graphemes(true).count();
            prop_assert_eq!(
                clusters,
                1,
                "'{}' measures {} cells at width {} and has {} clusters: overflow is only \
                 allowed for a single indivisible cluster",
                line.escape_debug(),
                measured,
                width,
                clusters
            );
        }
    }

    #[test]
    fn grapheme_clusters_are_never_split(text in varied_text(), width in 1usize..60) {
        // The input's cluster sequence is compared against the output's: had a break split
        // one, a cluster that was not in the input would show up.
        let input: Vec<&str> = text.graphemes(true).collect();
        let output = wrap(&text, width);
        let joined: String = output.concat();

        for cluster in joined.graphemes(true) {
            prop_assert!(
                input.contains(&cluster) || cluster.chars().all(char::is_whitespace),
                "cluster '{}' appeared, and it was not in the input",
                cluster.escape_debug()
            );
        }
    }

    #[test]
    fn no_visible_text_is_lost(text in varied_text(), width in 1usize..60) {
        // A viewer does not edit. Spaces may legitimately change — reflowing is its job —
        // but no non-space character may disappear or appear.
        let expected: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        let got: String = wrap(&text, width)
            .concat()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        prop_assert_eq!(&expected, &got, "the visible text changed at width {}", width);
    }

    #[test]
    fn no_line_ends_with_a_space(text in varied_text(), width in 1usize..60) {
        for line in wrap(&text, width) {
            prop_assert_eq!(
                line.trim_end(),
                &line,
                "'{}' ends with a space",
                line.escape_debug()
            );
        }
    }

    #[test]
    fn the_prefix_is_always_present_and_counted(text in varied_text(), width in 4usize..60) {
        let prefix = vec![Segment::plain("> ")];
        let mut buf = WrapBuffer::new();
        buf.push_text(Cow::Borrowed(&text), Style::PLAIN, None);
        let lines = buf.wrap(width, &prefix, None);

        for line in &lines {
            let text_line = line.to_plain_string();
            prop_assert!(
                text_line.starts_with("> "),
                "'{}' lost its prefix",
                text_line.escape_debug()
            );
            prop_assert!(
                display_width(&text_line) <= width,
                "'{}' overflows once the prefix is counted",
                text_line.escape_debug()
            );
        }
    }

    #[test]
    fn the_declared_width_matches_the_measured_one(text in varied_text(), width in 1usize..60) {
        // The backend trusts `line.width` and does not measure again. If it lies, table
        // alignment breaks in ways that are hard to track down.
        let mut buf = WrapBuffer::new();
        buf.push_text(Cow::Borrowed(&text), Style::PLAIN, None);
        for line in buf.wrap(width, &[], None) {
            let measured = display_width(&line.to_plain_string());
            prop_assert_eq!(
                line.width,
                measured,
                "declared {} but measures {}",
                line.width,
                measured
            );
        }
    }

    #[test]
    fn it_never_loops_or_emits_unbounded_empty_lines(
        text in varied_text(),
        width in 1usize..5,
    ) {
        // Minimal widths are where a badly written wrapper hangs.
        let lines = wrap(&text, width);
        let clusters = text.graphemes(true).count();
        prop_assert!(
            lines.len() <= clusters + 1,
            "{} lines for {clusters} clusters: there is a split making no progress",
            lines.len()
        );
    }
}
