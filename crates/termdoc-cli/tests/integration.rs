//! Integration tests for the binary: M0's acceptance criteria.
//!
//! These are the ones that check termdoc behaves like a system utility rather than merely
//! that its functions return the right values. The SIGPIPE one is the most important test in
//! all of M0: if it fails, termdoc cannot be composed with anything.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_termdoc")
}

fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../corpus")
        .join(name)
}

/// Runs termdoc with stdout captured — that is, in pipe mode.
fn run(args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin())
        .args(args)
        // The relevant environment is cleared so the machine running the tests cannot change
        // the outcome.
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("COLUMNS")
        .output()
        .expect("the binary must run");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn run_stdin(args: &[&str], input: &str) -> (String, String, i32) {
    let mut child = Command::new(bin())
        .args(args)
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary must start");
    child
        .stdin
        .as_mut()
        .expect("stdin connected")
        .write_all(input.as_bytes())
        .expect("writing to stdin");
    let out = child.wait_with_output().expect("waiting for the process");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

// ---------------------------------------------------------------- SIGPIPE

/// M0's critical criterion.
///
/// Rust ignores `SIGPIPE` at startup, so without the reset in `main` this would end with a
/// broken-pipe panic and garbage on stderr. `cat` does not do that; neither should termdoc.
#[test]
fn closing_the_pipe_early_does_not_panic() {
    let dir = std::env::temp_dir().join("termdoc-test-sigpipe");
    std::fs::create_dir_all(&dir).expect("temp directory");
    let path = dir.join("large.log");

    // It has to comfortably exceed the pipe buffer (typically 64 KiB) so the write actually
    // blocks and the process receives the signal.
    if !path.exists() {
        let mut f = std::fs::File::create(&path).expect("creating the log");
        for i in 0..200_000 {
            writeln!(f, "line {i} with enough filler to really take up space")
                .expect("writing the log");
        }
    }

    let mut child = Command::new(bin())
        .arg(&path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("startup");

    let stdout = child.stdout.take().expect("stdout connected");
    let mut reader = BufReader::new(stdout);
    let mut read = 0;
    for _ in 0..5 {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        read += 1;
    }
    // Closing the read end is what triggers SIGPIPE in the writer.
    drop(reader);

    let out = child.wait_with_output().expect("waiting for the process");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(read, 5, "it should have emitted at least five lines");
    assert!(
        !stderr.contains("panicked"),
        "it panicked when the pipe closed:\n{stderr}"
    );
    assert!(
        !stderr.contains("Broken pipe") && !stderr.contains("os error 32"),
        "the pipe error leaked to stderr:\n{stderr}"
    );
    assert!(
        stderr.is_empty(),
        "stderr should have stayed clean, it contained:\n{stderr}"
    );
}

// ---------------------------------------------------------------- clean output

#[test]
fn no_escapes_are_emitted_into_a_pipe() {
    // The invariant that makes `termdoc x.md > f` produce a usable file.
    let (stdout, _, code) = run(&[corpus("basic.md").to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(
        !stdout.contains('\x1b'),
        "escape sequences leaked into a pipe"
    );
    assert!(stdout.contains("termdoc"), "and the content must be there");
}

/// Precedence between `NO_COLOR` and `--color`.
///
/// This follows the ecosystem's convention (ripgrep, bat, delta): **flags beat the
/// environment**. `NO_COLOR` is a persistent machine-level preference; `--color always` is an
/// explicit, immediate instruction from whoever runs the command, and the latter should be
/// able to override the former without having to scrub the environment.
///
/// With `--color auto` — the default — `NO_COLOR` is always honored.
#[test]
fn no_color_is_honored_with_color_auto() {
    let out = Command::new(bin())
        .args([corpus("basic.md").to_str().unwrap()])
        .env("NO_COLOR", "1")
        .env("CLICOLOR_FORCE", "1")
        .output()
        .expect("execution");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains('\x1b'),
        "with --color auto, NO_COLOR must beat even CLICOLOR_FORCE"
    );
}

#[test]
fn color_always_overrides_no_color() {
    let out = Command::new(bin())
        .args(["--color", "always", corpus("basic.md").to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("execution");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains('\x1b'),
        "an explicit flag must be able to override the environment"
    );
}

#[test]
fn color_always_does_emit_escapes() {
    let (stdout, _, _) = run(&["--color", "always", corpus("basic.md").to_str().unwrap()]);
    assert!(stdout.contains('\x1b'), "color was requested explicitly");

    // No style may survive a newline. We do not check that the line *ends* with a reset — it
    // may legitimately end with unstyled text — but that the last SGR sequence on the line is
    // a reset. Every SGR the backend emits starts from zero, so that final sequence
    // determines the resulting state.
    for line in stdout.lines().filter(|l| l.contains('\x1b')) {
        let last = line
            .rmatch_indices("\x1b[")
            .next()
            .map(|(i, _)| &line[i..])
            .expect("there is at least one sequence");
        assert!(
            last.starts_with("\x1b[0m"),
            "the last SGR of '{}' is not a reset: '{}'",
            line.escape_debug(),
            last.escape_debug()
        );
    }
}

// ---------------------------------------------------------------- degradation

#[test]
fn ascii_leaves_not_one_non_ascii_byte() {
    let (stdout, _, code) = run(&["--ascii", corpus("tables.md").to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(
        stdout.is_ascii(),
        "with --ascii the output must be pure ASCII:\n{stdout}"
    );
}

#[test]
fn the_width_is_honored() {
    for width in [20usize, 40, 60] {
        let (stdout, _, _) = run(&[
            "--width",
            &width.to_string(),
            corpus("basic.md").to_str().unwrap(),
        ]);
        for line in stdout.lines() {
            let measured = display_width(line);
            assert!(
                measured <= width,
                "at --width {width}, '{line}' measures {measured}"
            );
        }
    }
}

#[test]
fn the_content_survives_every_rung() {
    // Degrading changes the appearance, never what the document says.
    let path = corpus("basic.md").to_str().unwrap().to_string();
    let variants: Vec<Vec<&str>> = vec![
        vec![&path],
        vec!["--ascii", &path],
        vec!["--color", "always", &path],
        vec!["-t", "plain", &path],
        vec!["--width", "40", &path],
    ];

    for args in variants {
        let (stdout, _, code) = run(&args);
        assert_eq!(code, 0, "{args:?}");
        let visible = strip_escapes(&stdout);
        for word in ["termdoc", "Features", "Markdown", "pending"] {
            assert!(visible.contains(word), "'{word}' vanished with {args:?}");
        }
    }
}

// ---------------------------------------------------------------- stdin

#[test]
fn markdown_is_detected_on_stdin_without_a_filename() {
    let (stdout, _, code) = run_stdin(&[], "# Title\n\nA paragraph.\n");
    assert_eq!(code, 0);
    assert!(stdout.contains("Title"));
    // Without color, the heading recovers its notation so it is not mistaken for a paragraph.
    assert!(stdout.contains("# Title"), "output:\n{stdout}");
}

#[test]
fn plain_text_on_stdin_is_not_treated_as_markdown() {
    let (stdout, _, code) = run_stdin(&[], "just text\nwith two lines\n");
    assert_eq!(code, 0);
    assert_eq!(stdout, "just text\nwith two lines\n");
}

#[test]
fn plain_text_line_breaks_are_preserved() {
    // In preformatted text the source's line breaks are meaningful and must not be joined.
    let (stdout, _, _) = run_stdin(&[], "one\ntwo\nthree\n");
    assert_eq!(stdout, "one\ntwo\nthree\n");
}

// ---------------------------------------------------------------- detection and encoding

/// A scratch file in a directory unique to this call.
///
/// The uniqueness matters: cargo runs tests in parallel, and two of them sharing a fixture path
/// means one can open the file in the instant the other has truncated it to zero bytes.
fn scratch(name: &str, content: &[u8]) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("termdoc-it-{n}"));
    std::fs::create_dir_all(&dir).expect("directory");
    let path = dir.join(name);
    std::fs::write(&path, content).expect("write");
    path
}

#[test]
fn every_m1_format_is_detected() {
    let cases: &[(&str, &[u8], &str)] = &[
        ("a.json", br#"{"a": 1}"#, "json"),
        ("a.yaml", b"name: x\n", "yaml"),
        ("a.toml", b"[pkg]\nname = \"x\"\n", "toml"),
        ("a.csv", b"a,b,c\n1,2,3\n4,5,6\n7,8,9\n", "csv"),
        ("a.xml", b"<?xml version=\"1.0\"?><r/>", "xml"),
        ("a.html", b"<!DOCTYPE html><html></html>", "html"),
        ("a.rs", b"fn main() {}", "code"),
        (
            "a.log",
            b"2026-08-10T12:00:00Z INFO up\n2026-08-10T12:00:01Z INFO ok\n",
            "log",
        ),
    ];
    for (name, content, expected) in cases {
        let path = scratch(name, content);
        let (stdout, _, code) = run(&["--explain", path.to_str().unwrap()]);
        assert_eq!(code, 0, "{name}");
        assert!(
            stdout.contains(&format!("format:   {expected}")),
            "{name} was not detected as {expected}:\n{stdout}"
        );
    }
}

#[test]
fn content_outranks_the_extension() {
    // JSON in a `.txt` file. Content is more reliable than a name when the two disagree.
    let path = scratch("data.txt", br#"{"a": 1, "b": [2, 3]}"#);
    let (stdout, _, _) = run(&["--explain", path.to_str().unwrap()]);
    assert!(stdout.contains("format:   json"), "{stdout}");
}

#[test]
fn a_readme_is_never_mistaken_for_yaml() {
    // The regression the YAML rule exists for: a Markdown file documenting options looks
    // exactly like YAML to a naive sniffer.
    let path = scratch(
        "README.md",
        b"# Options\n\nname: what to call it\nport: which one\n",
    );
    let (stdout, _, _) = run(&["--explain", path.to_str().unwrap()]);
    assert!(stdout.contains("format:   markdown"), "{stdout}");
}

#[test]
fn a_shebang_identifies_a_script_with_no_extension() {
    let path = scratch("deploy", b"#!/usr/bin/env python3\nprint(1)\n");
    let (stdout, _, _) = run(&["--explain", path.to_str().unwrap()]);
    assert!(stdout.contains("format:   code"), "{stdout}");
    assert!(
        stdout.contains("shebang"),
        "the reason must say why: {stdout}"
    );
}

#[test]
fn latin1_is_decoded_rather_than_mangled() {
    // End to end: detection resolves windows-1252, the source applies it, and the reader honors
    // it. Any one of the three failing produces replacement characters.
    let path = scratch(
        "latin1.txt",
        b"Comit\xE9 de direcci\xF3n, se\xF1or, ni\xF1o, a\xF1o\n",
    );
    let (stdout, stderr, code) = run(&[path.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(stdout.contains("Comité"), "stdout: {stdout:?}");
    assert!(stdout.contains("dirección"), "stdout: {stdout:?}");
    assert!(
        !stdout.contains('\u{FFFD}'),
        "no replacement characters expected: {stdout:?}"
    );
    assert!(
        stderr.is_empty(),
        "a correct decode needs no warning: {stderr}"
    );
}

#[test]
fn forcing_the_wrong_encoding_warns_but_still_shows_the_file() {
    let path = scratch("latin1.txt", b"caf\xE9\n");
    let (stdout, stderr, code) = run(&["--encoding", "utf-8", path.to_str().unwrap()]);
    assert_eq!(code, 0, "it must still render");
    assert!(stdout.contains("caf"), "{stdout:?}");
    assert!(stderr.contains("warning"), "and it must say so: {stderr}");
}

#[test]
fn an_unknown_encoding_is_refused_with_a_hint() {
    let path = scratch("a.txt", b"x");
    let (_, stderr, code) = run(&["--encoding", "not-real", path.to_str().unwrap()]);
    assert_eq!(code, 4, "stderr: {stderr}");
    assert!(
        stderr.contains("latin1"),
        "the error must suggest valid labels: {stderr}"
    );
}

#[test]
fn a_format_without_a_reader_still_shows_its_content() {
    // Mid-roadmap, detection names more formats than there are readers. Refusing to display a
    // .html file merely because its reader has not landed would be worse than showing it as
    // text, and it contradicts the point of a universal viewer.
    //
    // This used to use JSON, then YAML, TOML and XML, which gained readers in M1. Whichever format stands in here has
    // to be one with no reader yet, so this test moves down the roadmap as readers land, and
    // it disappears entirely once every detected text format has one.
    let path = scratch(
        "a.html",
        b"<!DOCTYPE html>\n<html><body>value</body></html>\n",
    );
    let (stdout, stderr, code) = run(&[path.to_str().unwrap()]);
    assert_eq!(code, 0, "it must not be an error");
    assert!(
        stdout.contains("<body>value</body>"),
        "the content must be there: {stdout}"
    );
    assert!(
        stderr.contains("no html reader yet"),
        "and it must explain why it looks plain: {stderr}"
    );
    // The message must be one clean line, with no source-indentation leaking into it.
    assert!(
        !stderr.contains("   "),
        "the warning has stray indentation: {stderr:?}"
    );
}

#[test]
fn strict_turns_the_fallback_warning_into_a_failure() {
    let path = scratch("a.html", b"<!DOCTYPE html>\n<html><body>1</body></html>\n");
    let (_, _, code) = run(&["--strict", path.to_str().unwrap()]);
    assert_eq!(code, 1, "--strict promotes warnings to errors");
}

#[test]
fn a_binary_is_never_dumped_as_text() {
    let path = scratch(
        "a.bin",
        &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0],
    );
    let (stdout, stderr, code) = run(&[path.to_str().unwrap()]);
    assert_ne!(code, 0, "a binary has no reader and must say so");
    assert!(stdout.is_empty(), "nothing may reach stdout: {stdout:?}");
    assert!(stderr.contains("termdoc:"), "{stderr}");
}

// ---------------------------------------------------------------- exit codes

#[test]
fn a_missing_file_gives_code_4() {
    let (_, stderr, code) = run(&["/does/not/exist/at/all.md"]);
    assert_eq!(code, 4, "stderr: {stderr}");
    assert!(stderr.contains("termdoc:"), "it must explain: {stderr}");
}

#[test]
fn an_invalid_format_gives_code_2() {
    let (_, stderr, code) = run(&[
        "--from",
        "nonexistent",
        corpus("basic.md").to_str().unwrap(),
    ]);
    assert_eq!(code, 2, "stderr: {stderr}");
    assert!(
        stderr.contains("markdown"),
        "the error must list the valid formats: {stderr}"
    );
}

#[test]
fn a_correct_document_gives_code_0() {
    let (_, _, code) = run(&[corpus("basic.md").to_str().unwrap()]);
    assert_eq!(code, 0);
}

// ---------------------------------------------------------------- utilities

#[test]
fn explain_justifies_the_detection() {
    let (stdout, _, code) = run(&["--explain", corpus("basic.md").to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(stdout.contains("format:"));
    assert!(stdout.contains("markdown"));
    assert!(
        stdout.contains("extension"),
        "it must say why, not just what: {stdout}"
    );
}

#[test]
fn formats_lists_what_this_binary_can_read() {
    let (stdout, _, code) = run(&["--formats"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("markdown"));
    assert!(stdout.contains("text"));
}

#[test]
fn several_files_are_separated_by_a_header() {
    let a = corpus("basic.md").to_str().unwrap().to_string();
    let b = corpus("plain.txt").to_str().unwrap().to_string();
    let (stdout, _, code) = run(&[&a, &b]);
    assert_eq!(code, 0);
    assert!(stdout.contains("==> "), "expected headers:\n{stdout}");
    assert_eq!(stdout.matches("==> ").count(), 2);
}

#[test]
fn no_header_suppresses_the_headers() {
    let a = corpus("basic.md").to_str().unwrap().to_string();
    let b = corpus("plain.txt").to_str().unwrap().to_string();
    let (stdout, _, _) = run(&["--no-header", &a, &b]);
    assert!(!stdout.contains("==> "));
}

#[test]
fn an_empty_file_is_not_an_error() {
    let dir = std::env::temp_dir().join("termdoc-test-integration");
    std::fs::create_dir_all(&dir).expect("directory");
    let path = dir.join("empty.md");
    std::fs::write(&path, b"").expect("write");

    let (stdout, stderr, code) = run(&[path.to_str().unwrap()]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout, "", "an empty file produces empty output");

    std::fs::remove_file(&path).ok();
}

#[test]
fn line_numbers_numbers_preformatted_content() {
    let (stdout, _, _) = run_stdin(&["-n", "--from", "text"], "alpha\nbeta\n");
    assert!(stdout.contains('1'), "output:\n{stdout}");
    assert!(stdout.contains("alpha"));
}

// ---------------------------------------------------------------- helpers

fn display_width(s: &str) -> usize {
    // Counted by hand to avoid adding `unicode-width` as a test dependency of the CLI:
    // telling the East Asian wide range apart is enough.
    s.chars()
        .filter(|c| !c.is_control())
        .map(|c| {
            let cp = c as u32;
            let wide = (0x1100..=0x115F).contains(&cp)
                || (0x2E80..=0xA4CF).contains(&cp)
                || (0xAC00..=0xD7A3).contains(&cp)
                || (0xF900..=0xFAFF).contains(&cp)
                || (0xFF00..=0xFF60).contains(&cp)
                || (0xFFE0..=0xFFE6).contains(&cp)
                || (0x1F300..=0x1F64F).contains(&cp);
            let zero = (0x0300..=0x036F).contains(&cp) || cp == 0x200D;
            if zero {
                0
            } else if wide {
                2
            } else {
                1
            }
        })
        .sum()
}

fn strip_escapes(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                while let Some(&c) = chars.peek() {
                    chars.next();
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[test]
fn windows_line_endings_leave_no_cr() {
    // The text reader hands the layout `one\r\n`; the layout trims it. Neither a stray CR nor
    // a merged line may reach the output.
    let path = scratch("crlf.txt", b"one\r\ntwo\r\nthree");
    let (stdout, _, code) = run(&[path.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert_eq!(stdout, "one\ntwo\nthree\n");
}

#[test]
fn json_keys_and_values_share_a_line_but_not_a_style() {
    let (stdout, _, code) = run(&["--color", "always", corpus("data.json").to_str().unwrap()]);
    assert_eq!(code, 0);
    // More than one SGR run on a line is the whole point: keys and scalars differ.
    let styled_line = stdout
        .lines()
        .any(|l| l.matches('\x1b').count() >= 4 && l.contains(':'));
    assert!(styled_line, "{stdout:?}");
}

#[test]
fn json_has_no_escape_bytes_without_color() {
    let (stdout, _, _) = run(&["--color", "never", corpus("data.json").to_str().unwrap()]);
    assert!(!stdout.contains('\x1b'), "{stdout:?}");
}

#[test]
fn yaml_keeps_its_comments_and_colors_its_keys() {
    let path = scratch("c.yaml", b"# the name\nname: termdoc # inline\nport: 80\n");
    let p = path.to_str().unwrap();
    let (plain, _, code) = run(&["--color", "never", p]);
    assert_eq!(code, 0);
    assert_eq!(plain, "# the name\nname: termdoc # inline\nport: 80\n");
    let (colored, stderr, _) = run(&["--color", "always", p]);
    assert!(stderr.is_empty(), "{stderr}");
    assert!(colored.contains('\x1b'), "{colored:?}");
}

#[test]
fn toml_keeps_its_comments_and_colors_its_keys() {
    let src = b"# the package\n[package]\nname = \"termdoc\" # inline\nn = [\n  1,\n]\n";
    let path = scratch("c.toml", src);
    let p = path.to_str().unwrap();
    let (plain, _, code) = run(&["--color", "never", p]);
    assert_eq!(code, 0);
    assert_eq!(plain.as_bytes(), src);
    let (colored, stderr, _) = run(&["--color", "always", p]);
    assert!(stderr.is_empty(), "{stderr}");
    assert!(colored.contains('\x1b'), "{colored:?}");
}

#[test]
fn xml_keeps_its_comments_and_colors_its_tags() {
    let src = b"<?xml version=\"1.0\"?>\n<!-- note -->\n<a id=\"1\">x &amp; y</a>\n";
    let path = scratch("c.xml", src);
    let p = path.to_str().unwrap();
    let (plain, _, code) = run(&["--color", "never", p]);
    assert_eq!(code, 0);
    assert_eq!(plain.as_bytes(), src);
    let (colored, stderr, _) = run(&["--color", "always", p]);
    assert!(stderr.is_empty(), "{stderr}");
    assert!(colored.contains('\x1b'), "{colored:?}");
}

#[test]
fn a_broken_xml_warns_and_still_shows_everything() {
    let src = b"<a>ok</a>\n<b attr=\"never closed";
    let path = scratch("b.xml", src);
    let (stdout, stderr, code) = run(&["--color", "never", path.to_str().unwrap()]);
    assert_eq!(code, 0, "partial rendering is not an error");
    assert!(stdout.contains("never closed"), "{stdout}");
    assert!(stderr.contains("not well-formed"), "{stderr}");
}

#[test]
fn csv_is_a_table_and_its_delimiter_is_detected() {
    let path = scratch("d.csv", b"name;qty\nbolt;3\nnut;15\nwasher;120\nscrew;7\n");
    let (stdout, stderr, code) =
        run(&["--color", "never", "--width", "60", path.to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.contains('┌'), "a table, not text: {stdout}");
    // The semicolon was a delimiter, not part of the content.
    assert!(!stdout.contains(';'), "{stdout}");
    // The numeric column is right-aligned: the shorter number is padded on its left.
    assert!(
        stdout
            .lines()
            .any(|l| l.contains("│   3 │") || l.contains("│   7 │")),
        "{stdout}"
    );
}

#[test]
fn a_tsv_extension_means_tabs_even_when_the_sample_is_too_small_to_tell() {
    let path = scratch("d.tsv", b"a\tb\n1\t2\n");
    let (stdout, _, code) = run(&["--color", "never", "--width", "60", path.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(stdout.contains('┌'), "{stdout}");
    assert!(!stdout.contains('\t'), "{stdout:?}");
}

#[test]
fn the_tsv_rung_still_carries_every_cell() {
    let path = scratch(
        "w.csv",
        b"alpha,beta,gamma\n1,2,3\n4,5,6\n7,8,9\n10,11,12\n",
    );
    let (stdout, _, code) = run(&["--color", "never", "--width", "6", path.to_str().unwrap()]);
    assert_eq!(code, 0);
    for cell in ["alpha", "gamma", "11", "12"] {
        assert!(stdout.contains(cell), "{cell} lost: {stdout}");
    }
}

#[test]
fn a_csv_past_the_ceiling_still_shows_every_record() {
    let mut data = String::from("id,v\n");
    for i in 0..20_000 {
        data.push_str(&format!("{i},x\n"));
    }
    let path = scratch("big.csv", data.as_bytes());
    let (stdout, stderr, code) =
        run(&["--color", "never", "--width", "40", path.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(stderr.contains("more than"), "{stderr}");
    // 12,500 is the ceiling for two columns; the last record is far beyond it.
    assert!(stdout.contains("19999,x"), "the tail was lost");
}

#[test]
fn delimiter_overrides_detection() {
    // A space is not among the delimiters detection will guess, so only the flag can say it.
    let path = scratch("o.csv", b"a b\n1 2\n");
    let p = path.to_str().unwrap();
    let (guess, _, _) = run(&["--color", "never", "--width", "60", p]);
    let (forced, stderr, code) = run(&[
        "--color",
        "never",
        "--width",
        "60",
        "--delimiter",
        "space",
        p,
    ]);
    assert_eq!(code, 0, "{stderr}");
    // Two columns, and the numeric ones are right-aligned to the three-cell minimum.
    assert!(forced.contains("│   a │   b │"), "{forced}");
    // Without the flag the space stayed inside one cell.
    assert!(guess.contains("a b"), "{guess}");
    assert!(!guess.contains("│   a │"), "{guess}");
}

#[test]
fn a_bad_delimiter_is_a_usage_error() {
    let path = scratch("p.csv", b"a,b\n1,2\n");
    let (_, stderr, code) = run(&["--delimiter", "ab", path.to_str().unwrap()]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("not a delimiter"), "{stderr}");
}

#[test]
fn a_numeric_first_row_is_data_unless_told_otherwise() {
    let path = scratch("n.csv", b"2021,2022\n10,20\n30,40\n");
    let p = path.to_str().unwrap();
    let (auto, _, _) = run(&["--color", "never", "--width", "60", p]);
    let (header, _, _) = run(&[
        "--color",
        "never",
        "--width",
        "60",
        "--csv-header",
        "yes",
        p,
    ]);
    // A header is followed by a rule; data is not.
    let rules = |s: &str| s.lines().filter(|l| l.starts_with('├')).count();
    assert_eq!(rules(&auto), 0, "{auto}");
    assert_eq!(rules(&header), 1, "{header}");
}

#[test]
fn no_header_still_means_no_filename_banner() {
    // `--no-header` predates the CSV flag and is about the `==> file <==` banners.
    let a = scratch("a.csv", b"x,y\n1,2\n");
    let b = scratch("b.csv", b"x,y\n3,4\n");
    let (out, _, code) = run(&["--no-header", a.to_str().unwrap(), b.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(!out.contains("==>"), "{out}");
}

#[test]
fn a_source_file_is_highlighted_with_colour_and_untouched_without() {
    let src = "fn main() {\n    let x = 1; // note\n}\n";
    let path = scratch("hl.rs", src.as_bytes());
    let p = path.to_str().unwrap();

    let (plain, stderr, code) = run(&["--color", "never", p]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(plain, src, "a pipe gets the file, byte for byte");

    let (colored, stderr, _) = run(&["--color", "always", p]);
    assert!(stderr.is_empty(), "{stderr}");
    // The keyword is magenta, and the comment is not: roles, not one flat colour.
    assert!(colored.contains("\x1b[0;35mfn"), "{colored:?}");
    assert!(colored.contains("// note"), "{colored:?}");
    assert!(
        colored.matches('\x1b').count() > 8,
        "several roles on a few lines: {colored:?}"
    );
}

#[test]
fn fenced_code_is_highlighted_only_when_its_language_has_a_grammar() {
    let md = "```python\ndef f(): pass\n```\n\n```nonsense-lang-xyz\ndef g(): pass\n```\n";
    let path = scratch("fence.md", md.as_bytes());
    let (out, stderr, code) = run(&["--color", "always", path.to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");

    let flat_yellow = "\x1b[0;93m";
    let python = out
        .lines()
        .find(|l| l.contains("def") && l.contains("f"))
        .unwrap();
    let unknown = out.lines().find(|l| l.contains("def g")).unwrap();
    // The block with a grammar gets roles; the one without keeps the theme's flat colour.
    assert!(!python.contains(flat_yellow), "{python:?}");
    assert!(python.contains("\x1b[0;35mdef"), "{python:?}");
    assert!(unknown.starts_with(flat_yellow), "{unknown:?}");
}

#[test]
fn a_pipe_never_pays_for_highlighting_and_never_changes() {
    let md = "```rust\nfn main() {}\n```\n";
    let path = scratch("pipe.md", md.as_bytes());
    let (out, _, code) = run(&[path.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert_eq!(out, "fn main() {}\n");
}

#[test]
fn line_numbers_still_work_on_highlighted_code() {
    let path = scratch("nl.rs", b"fn a() {}\nfn b() {}\n");
    let (out, _, code) = run(&["--color", "always", "--ascii", "-n", path.to_str().unwrap()]);
    assert_eq!(code, 0);
    let numbered: Vec<&str> = out.lines().filter(|l| l.contains(" | ")).collect();
    assert_eq!(numbered.len(), 2, "{out:?}");
    assert!(numbered[1].contains('2'), "{out:?}");
}

// ---------------------------------------------------------------- live input

/// Spawns termdoc reading a pipe that stays open, and gives back its stdin and a channel of the
/// lines it prints. Reading on a thread is what lets a test say "this line arrived within five
/// seconds" instead of hanging when it did not.
fn spawn_live(
    args: &[&str],
) -> (
    std::process::Child,
    std::process::ChildStdin,
    std::sync::mpsc::Receiver<String>,
) {
    use std::io::BufRead;
    let mut child = Command::new(bin())
        .args(args)
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the binary must start");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            // Stops when the program ends or when the test no longer listens.
            match line {
                Ok(l) => {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    (child, stdin, rx)
}

fn next_line(rx: &std::sync::mpsc::Receiver<String>, what: &str) -> String {
    rx.recv_timeout(std::time::Duration::from_secs(10))
        .unwrap_or_else(|_| panic!("no output within 10 s: {what}"))
}

#[test]
fn a_live_pipe_is_shown_as_it_arrives_not_when_it_ends() {
    // The point of incremental stdin, and the thing `kubectl logs -f | termdoc` needs: a line
    // written to a pipe that is still open comes out, without waiting for the end.
    let (mut child, mut stdin, rx) = spawn_live(&["--color", "never"]);

    stdin
        .write_all(b"2026-08-10 12:00:00 INFO first\n")
        .unwrap();
    stdin.flush().unwrap();
    assert_eq!(
        next_line(&rx, "the first line"),
        "2026-08-10 12:00:00 INFO first"
    );

    // And again, much later than the detection's grace period: the stream is still live.
    std::thread::sleep(std::time::Duration::from_millis(300));
    stdin
        .write_all(b"2026-08-10 12:00:01 WARN second\n")
        .unwrap();
    stdin.flush().unwrap();
    assert_eq!(
        next_line(&rx, "the second line"),
        "2026-08-10 12:00:01 WARN second"
    );

    drop(stdin);
    assert!(child.wait().unwrap().success());
}

#[test]
fn a_partial_line_waits_for_its_newline() {
    let (mut child, mut stdin, rx) = spawn_live(&["--color", "never"]);
    stdin.write_all(b"half a li").unwrap();
    stdin.flush().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        rx.try_recv().is_err(),
        "a line that has not ended was shown"
    );
    stdin.write_all(b"ne\n").unwrap();
    stdin.flush().unwrap();
    assert_eq!(next_line(&rx, "the completed line"), "half a line");
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

#[test]
fn an_endless_input_does_not_hang_the_program() {
    // `yes | termdoc | head`: before incremental stdin this read forever and printed nothing.
    let (mut child, mut stdin, rx) = spawn_live(&["--color", "never"]);
    let producer = std::thread::spawn(move || {
        let chunk = "2026-08-10 12:00:00 INFO an endless stream of lines\n".repeat(200);
        // Ends when the program is killed and the pipe breaks.
        while stdin.write_all(chunk.as_bytes()).is_ok() {}
    });
    for i in 0..5 {
        let line = next_line(&rx, &format!("line {i} of an endless input"));
        assert!(line.contains("INFO"), "{line}");
    }
    child.kill().unwrap();
    let _ = child.wait();
    producer.join().unwrap();
}

#[test]
fn a_stream_is_coloured_by_level_when_colour_is_forced() {
    let input = "2026-08-10 12:00:00 INFO ok\n\
                 2026-08-10 12:00:01 WARN careful\n\
                 2026-08-10 12:00:02 ERROR boom\n\
                 2026-08-10 12:00:03 INFO fine again\n";
    // Four timestamped lines are enough for detection to call it a log, with no flag.
    let (out, stderr, code) = run_stdin(&["--color", "always"], input);
    assert_eq!(code, 0, "{stderr}");
    // Bright green for INFO, bright yellow for WARN, bright red for ERROR: the levels carry
    // the colour.
    assert!(out.contains("\x1b[0;92mINFO"), "{out:?}");
    assert!(out.contains("\x1b[0;93mWARN"), "{out:?}");
    assert!(out.contains("\x1b[0;1;91mERROR"), "{out:?}");
}

#[test]
fn a_stream_too_short_to_detect_can_still_be_told_what_it_is() {
    // One or two lines are not enough evidence for detection, and a live stream often starts
    // with one. `--from` is the answer.
    let input = "2026-08-10 12:00:00 ERROR boom\n";
    let (guess, _, _) = run_stdin(&["--color", "always"], input);
    assert!(!guess.contains('\x1b'), "{guess:?}");
    let (told, _, code) = run_stdin(&["--color", "always", "--from", "log"], input);
    assert_eq!(code, 0);
    assert!(told.contains("\x1b[0;1;91mERROR"), "{told:?}");
}

#[test]
fn formats_that_need_the_whole_document_still_get_it_from_a_pipe() {
    // Markdown cannot be shown a line at a time; stdin must still deliver all of it.
    let (out, _, code) = run_stdin(&["--color", "never"], "# Title\n\nbody **bold**\n");
    assert_eq!(code, 0);
    assert!(out.contains("# Title") && out.contains("bold"), "{out}");
}
