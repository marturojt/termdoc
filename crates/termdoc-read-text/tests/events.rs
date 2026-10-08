//! Golden tests for the event stream.
//!
//! These assert the sequence of `Event`s **with no ANSI in the way**. That is what separates
//! a parsing bug from a painting bug: if a heading comes out wrong, these tests say whether
//! the reader misread it or the layout mislaid it, without any guessing.

use termdoc_core::{DocumentReader, Event, ReadContext, Source, Tag, TagKind};
use termdoc_read_text::{MarkdownReader, TextReader};

/// An indented textual rendering of the stream, readable in a diff.
fn dump(src: &Source, reader: &dyn DocumentReader) -> String {
    let mut out = String::new();
    let mut depth = 0usize;

    for event in reader.read(src, &ReadContext::default()).expect("read") {
        let event = event.expect("valid event");
        let line = match &event.node {
            Event::Start(tag) => {
                let s = format!("{}{}", "  ".repeat(depth), describe_tag(tag));
                depth += 1;
                s
            }
            Event::End(kind) => {
                depth = depth.saturating_sub(1);
                format!("{}/{}", "  ".repeat(depth), describe_kind(*kind))
            }
            other => format!("{}{}", "  ".repeat(depth), describe_leaf(other)),
        };
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn describe_tag(tag: &Tag<'_>) -> String {
    match tag {
        Tag::Document(_) => "Document".into(),
        Tag::Heading { level, .. } => format!("Heading({level})"),
        Tag::Paragraph => "Paragraph".into(),
        Tag::Preformatted => "Preformatted".into(),
        Tag::List { ordered, start, .. } => format!("List(ordered={ordered}, start={start})"),
        Tag::ListItem { marker } => format!("ListItem({marker:?})"),
        Tag::Table { align } => format!("Table({align:?})"),
        Tag::TableHead => "TableHead".into(),
        Tag::TableRow => "TableRow".into(),
        Tag::TableCell { .. } => "TableCell".into(),
        Tag::CodeBlock { lang, .. } => format!("CodeBlock({lang:?})"),
        Tag::BlockQuote { .. } => "BlockQuote".into(),
        Tag::Admonition { kind } => format!("Admonition({kind:?})"),
        Tag::Emphasis => "Emphasis".into(),
        Tag::Strong => "Strong".into(),
        Tag::Strikethrough => "Strikethrough".into(),
        Tag::Link { href, .. } => format!("Link({href})"),
        other => format!("{:?}", other.kind()),
    }
}

fn describe_kind(kind: TagKind) -> String {
    format!("{kind:?}")
}

fn describe_leaf(event: &Event<'_>) -> String {
    match event {
        Event::Text(t) => format!("Text({t:?})"),
        Event::Code(t) => format!("Code({t:?})"),
        Event::Break(k) => format!("Break({k:?})"),
        Event::Rule => "Rule".into(),
        Event::TaskMarker(b) => format!("TaskMarker({b})"),
        Event::Image { alt, .. } => format!("Image(alt={alt:?})"),
        Event::FootnoteRef(id) => format!("FootnoteRef({id})"),
        Event::Diagnostic(d) => format!("Diagnostic({:?})", d.severity),
        other => format!("{other:?}"),
    }
}

fn md(input: &str) -> String {
    let src = Source::from_bytes("t.md", input);
    dump(&src, &MarkdownReader::new())
}

fn txt(input: &str) -> String {
    let src = Source::from_bytes("t.txt", input);
    dump(&src, &TextReader::plain())
}

// ---------------------------------------------------------------- structure

#[test]
fn heading_and_paragraph() {
    assert_eq!(
        md("# Title\n\nA paragraph.\n"),
        "\
Document
  Heading(1)
    Text(\"Title\")
  /Heading
  Paragraph
    Text(\"A paragraph.\")
  /Paragraph
/Document
"
    );
}

#[test]
fn inline_containers_open_and_close() {
    // The reason the model was refined: emphasis wraps content, so it needs an open and a
    // close. With a single inline event there would be nowhere for it to end.
    assert_eq!(
        md("Some **bold** and some *italic*.\n"),
        "\
Document
  Paragraph
    Text(\"Some \")
    Strong
      Text(\"bold\")
    /Strong
    Text(\" and some \")
    Emphasis
      Text(\"italic\")
    /Emphasis
    Text(\".\")
  /Paragraph
/Document
"
    );
}

#[test]
fn ordered_lists_arrive_numbered() {
    // `pulldown-cmark` does not number items: the reader does, which is why the number has
    // to show up here.
    let dump = md("1. one\n2. two\n");
    assert!(dump.contains("ListItem(Ordered { number: 1 })"), "{dump}");
    assert!(dump.contains("ListItem(Ordered { number: 2 })"), "{dump}");
}

#[test]
fn ordered_lists_honor_their_start() {
    let dump = md("5. five\n6. six\n");
    assert!(dump.contains("start=5"), "{dump}");
    assert!(dump.contains("ListItem(Ordered { number: 5 })"), "{dump}");
}

#[test]
fn nested_bullets_carry_their_depth() {
    // Depth is what lets the layout rotate the glyph.
    let dump = md("- one\n  - two\n    - three\n");
    assert!(dump.contains("Bullet { depth: 0 }"), "{dump}");
    assert!(dump.contains("Bullet { depth: 1 }"), "{dump}");
    assert!(dump.contains("Bullet { depth: 2 }"), "{dump}");
}

#[test]
fn the_table_preserves_alignment() {
    let dump = md("| a | b | c |\n| :- | :-: | -: |\n| 1 | 2 | 3 |\n");
    assert!(dump.contains("Table([Left, Center, Right])"), "{dump}");
    assert!(dump.contains("TableHead"), "{dump}");
    assert!(dump.contains("TableRow"), "{dump}");
}

#[test]
fn the_code_block_preserves_its_language() {
    let dump = md("```rust\nfn main() {}\n```\n");
    assert!(dump.contains(r#"CodeBlock(Some("rust"))"#), "{dump}");
}

#[test]
fn the_language_is_taken_from_the_first_token() {
    // ```rust,ignore  →  the language is "rust", not "rust,ignore".
    let dump = md("```rust,ignore\nfn main() {}\n```\n");
    assert!(dump.contains(r#"CodeBlock(Some("rust"))"#), "{dump}");
}

#[test]
fn gfm_alerts_are_admonitions_not_quotes() {
    let dump = md("> [!WARNING]\n> Careful.\n");
    assert!(dump.contains("Admonition(Warning)"), "{dump}");
    assert!(!dump.contains("BlockQuote"), "{dump}");
}

#[test]
fn a_plain_quote_is_still_a_quote() {
    let dump = md("> Just a quote.\n");
    assert!(dump.contains("BlockQuote"), "{dump}");
    assert!(!dump.contains("Admonition"), "{dump}");
}

#[test]
fn an_image_is_a_leaf_with_its_complete_alt_text() {
    // In `pulldown` the alt text arrives as loose events between Start and End; the reader
    // joins them so the layout receives a single leaf.
    let dump = md("![a *diagram* of the system](a.png)\n");
    assert!(
        dump.contains(r#"Image(alt="a diagram of the system")"#),
        "{dump}"
    );
}

#[test]
fn tasks_carry_their_marker() {
    let dump = md("- [x] done\n- [ ] pending\n");
    assert!(dump.contains("TaskMarker(true)"), "{dump}");
    assert!(dump.contains("TaskMarker(false)"), "{dump}");
}

#[test]
fn raw_html_is_dropped_without_dirtying_the_text() {
    // Dumping `<br>` as text would be worse than omitting it; the M3 HTML reader is the one
    // that knows how to interpret it.
    let dump = md("Text <br> more text\n");
    assert!(!dump.contains("<br>"), "{dump}");
    assert!(dump.contains("Text"), "{dump}");
}

#[test]
fn the_link_preserves_its_target() {
    let dump = md("[text](https://example.com)\n");
    assert!(dump.contains("Link(https://example.com)"), "{dump}");
}

// ---------------------------------------------------------------- plain text

#[test]
fn plain_text_is_preformatted_with_one_line_per_event() {
    // Each line keeps its terminator: in `Preformatted` a newline is what closes a line.
    assert_eq!(
        txt("one\ntwo\n"),
        "\
Document
  Preformatted
    Text(\"one\\n\")
    Text(\"two\\n\")
  /Preformatted
/Document
"
    );
}

#[test]
fn plain_text_does_not_interpret_markers() {
    // A `#` in a .txt file is a `#`, not a heading.
    let dump = txt("# not a heading\n**nor bold**\n");
    assert!(!dump.contains("Heading"), "{dump}");
    assert!(!dump.contains("Strong"), "{dump}");
    // Odd but necessary: `r#"..."#` would terminate at the `"#` in the content.
    assert!(dump.contains(r##"Text("# not a heading\n")"##), "{dump}");
}

#[test]
fn the_stream_is_always_well_formed() {
    // Every open has its close, for any input. If this fails, the layout accumulates frames
    // and the indentation drifts without ever raising an error.
    let inputs = [
        "",
        "\n",
        "# a",
        "- a\n  - b\n",
        "| a |\n| - |\n| 1 |\n",
        "> [!TIP]\n> x\n",
        "```\nx\n```",
        "**unclosed",
        "![img](a.png)",
        "1. a\n\n   nested paragraph\n",
    ];

    for input in inputs {
        for (name, dump) in [("md", md(input)), ("txt", txt(input))] {
            let mut open = 0i32;
            for line in dump.lines() {
                let t = line.trim_start();
                if t.starts_with('/') {
                    open -= 1;
                } else if !t.starts_with("Text(")
                    && !t.starts_with("Code(")
                    && !t.starts_with("Break(")
                    && !t.starts_with("Rule")
                    && !t.starts_with("TaskMarker")
                    && !t.starts_with("Image(")
                    && !t.starts_with("FootnoteRef")
                    && !t.starts_with("Diagnostic")
                {
                    open += 1;
                }
                assert!(
                    open >= 0,
                    "{name}: a tag was closed that was never opened, for {input:?}\n{dump}"
                );
            }
            assert_eq!(
                open, 0,
                "{name}: {open} tags left open for {input:?}\n{dump}"
            );
        }
    }
}
