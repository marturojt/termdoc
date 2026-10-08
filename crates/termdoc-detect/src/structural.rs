//! Structural sniffing: deciding a format by looking at the content.
//!
//! This is the only route available for stdin, where there is no filename to lean on, so it
//! carries real weight. It is also where a wrong guess is most damaging, because content
//! outranks the extension.
//!
//! ## Which formats may claim STRUCTURAL confidence
//!
//! Only the ones whose prefix can be *checked* rather than guessed at:
//!
//! - **JSON** — the prefix actually parses as a value.
//! - **XML/HTML** — an `<?xml` declaration, a doctype, or a well-formed root tag.
//! - **TOML** — a `[section]` header, which nothing else uses.
//! - **CSV** — zero column-count variance across several lines (see `delimited`).
//!
//! **YAML deliberately does not.** `key: value` lines are indistinguishable from a Markdown
//! document listing options, from a log with `field: value` pairs, or from prose containing a
//! colon. Letting YAML claim STRUCTURAL (70) would beat a `.md` extension (50) and render a
//! README as YAML. So YAML only claims HEURISTIC, and only with an explicit document marker
//! (`---` or `%YAML`); otherwise it relies on its extension, which is what people actually
//! have.

use termdoc_core::{Detection, FormatId, confidence};

use crate::delimited;

/// Tries every structural check, most reliable first.
pub fn sniff(text: &str) -> Option<Detection> {
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(d) = sniff_xml(trimmed) {
        return Some(d);
    }
    if let Some(d) = sniff_json(trimmed) {
        return Some(d);
    }
    if let Some(d) = sniff_toml(text) {
        return Some(d);
    }
    if let Some(d) = sniff_csv(text) {
        return Some(d);
    }
    None
}

/// Weaker signals, only reached when nothing above matched.
pub fn sniff_heuristic(text: &str) -> Option<Detection> {
    if let Some(d) = sniff_markdown(text) {
        return Some(d);
    }
    if let Some(d) = sniff_yaml(text) {
        return Some(d);
    }
    if let Some(d) = sniff_log(text) {
        return Some(d);
    }
    None
}

// ---------------------------------------------------------------- XML / HTML

fn sniff_xml(trimmed: &str) -> Option<Detection> {
    let lower_start: String = trimmed
        .chars()
        .take(200)
        .flat_map(|c| c.to_lowercase())
        .collect();

    if lower_start.starts_with("<!doctype html") {
        return Some(Detection::new(
            FormatId::Html,
            confidence::STRUCTURAL,
            "HTML doctype",
        ));
    }
    if lower_start.starts_with("<html") {
        return Some(Detection::new(
            FormatId::Html,
            confidence::STRUCTURAL,
            "<html> root element",
        ));
    }
    if trimmed.starts_with("<?xml") {
        // An XML declaration may still introduce XHTML, so the root element decides.
        if lower_start.contains("<html") {
            return Some(Detection::new(
                FormatId::Html,
                confidence::STRUCTURAL,
                "XML declaration with an <html> root",
            ));
        }
        return Some(Detection::new(
            FormatId::Xml,
            confidence::STRUCTURAL,
            "XML declaration",
        ));
    }
    if trimmed.starts_with("<!DOCTYPE") {
        return Some(Detection::new(
            FormatId::Xml,
            confidence::STRUCTURAL,
            "SGML/XML doctype",
        ));
    }

    // A bare root tag: `<foo>` or `<foo ...>`, requiring a letter after the `<` so `<-` or
    // `< 3` in prose does not qualify.
    let mut chars = trimmed.chars();
    if chars.next() == Some('<')
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && trimmed.contains('>')
    {
        return Some(Detection::new(
            FormatId::Xml,
            confidence::STRUCTURAL,
            "opening root tag",
        ));
    }

    None
}

// ---------------------------------------------------------------- JSON

fn sniff_json(trimmed: &str) -> Option<Detection> {
    let first = trimmed.chars().next()?;
    if first != '{' && first != '[' {
        // A bare scalar is technically valid JSON, but claiming a whole file for `42` or
        // `"text"` would be absurd. Only objects and arrays count.
        return None;
    }

    // The prefix is checked for balance rather than fully parsed: the probe usually cuts the
    // document off partway, so a real parse would report an unexpected end of input for every
    // large file. Balanced-and-plausible is the right bar here.
    if !plausible_json(trimmed) {
        return None;
    }

    Some(Detection::new(
        FormatId::Json,
        confidence::STRUCTURAL,
        if first == '{' {
            "opens a JSON object"
        } else {
            "opens a JSON array"
        },
    ))
}

/// Walks the prefix checking that nothing forbidden by JSON shows up.
///
/// Returns `false` on the first structural impossibility. Truncation is *not* a failure.
fn plausible_json(text: &str) -> bool {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut seen_structure = false;

    for c in text.chars().take(4096) {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }

        match c {
            '"' => in_string = true,
            '{' | '[' => {
                depth += 1;
                seen_structure = true;
            }
            '}' | ']' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            // Everything JSON allows outside a string.
            ',' | ':' | '-' | '+' | '.' | 'e' | 'E' => {}
            c if c.is_ascii_digit() || c.is_whitespace() => {}
            // Only the three literals use bare letters.
            't' | 'r' | 'u' | 'f' | 'a' | 'l' | 's' | 'n' => {}
            // Anything else — a `#` comment, a bare key, a stray `'` — is not JSON.
            _ => return false,
        }
    }

    seen_structure
}

// ---------------------------------------------------------------- TOML

fn sniff_toml(text: &str) -> Option<Detection> {
    let mut sections = 0usize;
    let mut assignments = 0usize;

    for line in text.lines().take(200) {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        // A `[section]` or `[[array]]` header on its own line is unique to TOML among the
        // formats we handle.
        if t.starts_with('[') && t.ends_with(']') && t.len() > 2 {
            sections += 1;
            continue;
        }
        if let Some((key, _)) = t.split_once('=') {
            let key = key.trim();
            if !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '"' | '\''))
            {
                assignments += 1;
            }
        }
    }

    // A section header is decisive. Bare assignments are not: `.env` files, Makefiles and
    // INI all look the same, so those need an extension to back them up.
    if sections > 0 && assignments > 0 {
        return Some(Detection::new(
            FormatId::Toml,
            confidence::STRUCTURAL,
            "[section] header with assignments",
        ));
    }
    None
}

// ---------------------------------------------------------------- CSV

fn sniff_csv(text: &str) -> Option<Detection> {
    let d = delimited::detect(text)?;
    Some(Detection::new(
        FormatId::Csv,
        confidence::STRUCTURAL,
        format!(
            "{}-delimited, {} consistent columns over {} lines",
            d.delimiter_name(),
            d.columns,
            d.consistent_lines
        ),
    ))
}

// ---------------------------------------------------------------- Markdown

fn sniff_markdown(text: &str) -> Option<Detection> {
    for line in text.lines().take(200) {
        // An ATX heading: one to six `#` followed by a space. Column 0 is required, because a
        // mid-line `#` is a comment or a hash.
        let hashes = line.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') {
            return Some(Detection::new(
                FormatId::Markdown,
                confidence::HEURISTIC,
                "ATX heading in column 0",
            ));
        }
        if line.starts_with("```") || line.starts_with("~~~") {
            return Some(Detection::new(
                FormatId::Markdown,
                confidence::HEURISTIC,
                "code fence",
            ));
        }
        let t = line.trim();
        if t.starts_with('|') && t.contains("---") {
            return Some(Detection::new(
                FormatId::Markdown,
                confidence::HEURISTIC,
                "GFM table separator",
            ));
        }
    }
    None
}

// ---------------------------------------------------------------- YAML

/// YAML only with an explicit marker. See this module's header for why.
fn sniff_yaml(text: &str) -> Option<Detection> {
    for line in text.lines().take(20) {
        let t = line.trim_end();
        if t == "---" || t.starts_with("--- ") {
            return Some(Detection::new(
                FormatId::Yaml,
                confidence::HEURISTIC,
                "--- document marker",
            ));
        }
        if t.starts_with("%YAML") {
            return Some(Detection::new(
                FormatId::Yaml,
                confidence::HEURISTIC,
                "%YAML directive",
            ));
        }
        if !t.is_empty() && !t.starts_with('#') {
            // The marker has to come first: a `---` further down is a Markdown horizontal
            // rule or a front-matter terminator, not a YAML document start.
            break;
        }
    }
    None
}

// ---------------------------------------------------------------- logs

fn sniff_log(text: &str) -> Option<Detection> {
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(20)
        .collect();
    if lines.len() < 3 {
        return None;
    }

    // A log is recognized by *every* line starting the same way. One timestamped line proves
    // nothing; twenty of them is a log.
    let with_timestamp = lines.iter().filter(|l| starts_with_timestamp(l)).count();
    if with_timestamp * 100 / lines.len() >= 80 {
        return Some(Detection::new(
            FormatId::Log,
            confidence::HEURISTIC,
            "most lines begin with a timestamp",
        ));
    }
    None
}

/// Whether a line starts the way a log line does.
///
/// The strict shapes — ISO-8601, a clock, syslog — come from `termdoc_core::timestamp`, the same
/// parser the log reader colours with, so that "detected as a log" and "highlighted as a log"
/// cannot disagree. What is added here is looser on purpose, because detection only needs to
/// count lines that *begin* like log lines: a month name and a day, and a bracketed prefix.
fn starts_with_timestamp(line: &str) -> bool {
    // Indented lines are continuations (a stack trace), not the start of a record.
    if !line.starts_with([' ', '\t']) && termdoc_core::timestamp::leading_timestamp(line).is_some()
    {
        return true;
    }

    // `Aug 10 12:00:00`
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    if line.len() >= 6 && MONTHS.iter().any(|m| line.starts_with(m)) {
        let rest = &line[3..];
        if rest.starts_with(' ')
            && rest
                .trim_start()
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
        {
            return true;
        }
    }

    // `[2026-08-10 ...]` and `[INFO] ...`
    if let Some(inner) = line.strip_prefix('[')
        && inner.len() >= 4
        && inner.chars().next().is_some_and(|c| c.is_ascii_digit())
    {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------- JSON

    #[test]
    fn detects_a_json_object() {
        let d = sniff(r#"{"name": "termdoc", "version": 1}"#).expect("should detect");
        assert_eq!(d.format, FormatId::Json);
        assert_eq!(d.confidence, confidence::STRUCTURAL);
    }

    #[test]
    fn detects_a_json_array() {
        assert_eq!(sniff("[1, 2, 3]").unwrap().format, FormatId::Json);
    }

    #[test]
    fn truncated_json_is_still_json() {
        // The probe almost always cuts a large document off partway; a real parse would
        // reject every one of them.
        let d = sniff(r#"{"a": 1, "b": [2, 3, {"c": "unterminated"#).expect("should detect");
        assert_eq!(d.format, FormatId::Json);
    }

    #[test]
    fn a_bare_scalar_is_not_claimed_as_json() {
        assert!(sniff("42").is_none());
        assert!(sniff("\"just a string\"").is_none());
    }

    #[test]
    fn json_with_comments_is_rejected() {
        // JSONC and friends are not JSON; the `#` gives it away.
        assert!(sniff("{\n  # a comment\n  \"a\": 1\n}").is_none());
    }

    #[test]
    fn unbalanced_brackets_are_rejected() {
        assert!(sniff("{\"a\": 1}}").is_none());
    }

    #[test]
    fn braces_inside_strings_do_not_unbalance_it() {
        let d = sniff(r#"{"pattern": "a}b{c", "ok": true}"#).expect("should detect");
        assert_eq!(d.format, FormatId::Json);
    }

    #[test]
    fn an_escaped_quote_does_not_unbalance_it() {
        let d = sniff(r#"{"say": "a \" b", "n": 1}"#).expect("should detect");
        assert_eq!(d.format, FormatId::Json);
    }

    // ---------------- XML / HTML

    #[test]
    fn distinguishes_html_from_xml() {
        assert_eq!(
            sniff("<!DOCTYPE html>\n<html><body>x</body></html>")
                .unwrap()
                .format,
            FormatId::Html
        );
        assert_eq!(
            sniff("<?xml version=\"1.0\"?>\n<root><a/></root>")
                .unwrap()
                .format,
            FormatId::Xml
        );
    }

    #[test]
    fn xhtml_is_html_not_xml() {
        // An XML declaration introducing XHTML: the root element is what matters.
        let d = sniff("<?xml version=\"1.0\"?>\n<html xmlns=\"...\"><body/></html>").unwrap();
        assert_eq!(d.format, FormatId::Html);
    }

    #[test]
    fn a_bare_root_tag_is_xml() {
        assert_eq!(
            sniff("<config><item/></config>").unwrap().format,
            FormatId::Xml
        );
    }

    #[test]
    fn a_less_than_sign_in_prose_is_not_xml() {
        assert!(sniff("if a < b then something happens >").is_none());
        assert!(sniff("<- look at this arrow").is_none());
    }

    // ---------------- TOML

    #[test]
    fn detects_toml_by_its_section_header() {
        let d = sniff("[package]\nname = \"termdoc\"\nversion = \"0.1.0\"\n").unwrap();
        assert_eq!(d.format, FormatId::Toml);
        assert_eq!(d.confidence, confidence::STRUCTURAL);
    }

    #[test]
    fn bare_assignments_are_not_claimed_as_toml() {
        // A `.env` file, a Makefile and an INI all look identical here, so this needs the
        // extension to decide.
        assert!(sniff("KEY = value\nOTHER = thing\n").is_none());
    }

    // ---------------- CSV

    #[test]
    fn detects_csv_through_the_delimiter_layer() {
        let d = sniff("a,b,c\n1,2,3\n4,5,6\n7,8,9\n").unwrap();
        assert_eq!(d.format, FormatId::Csv);
        assert!(d.reason.contains("comma"), "{}", d.reason);
        assert!(
            d.reason.contains('3'),
            "the column count belongs in the reason"
        );
    }

    // ---------------- Markdown, YAML, logs

    #[test]
    fn markdown_is_heuristic_not_structural() {
        let d = sniff_heuristic("# Title\n\ntext\n").unwrap();
        assert_eq!(d.format, FormatId::Markdown);
        assert_eq!(d.confidence, confidence::HEURISTIC);
    }

    #[test]
    fn yaml_needs_an_explicit_marker() {
        // The decision documented in this module's header: `key: value` alone is not enough,
        // or a Markdown file listing options would render as YAML.
        assert!(sniff_heuristic("name: termdoc\nversion: 1\n").is_none());

        let d = sniff_heuristic("---\nname: termdoc\n").expect("--- is decisive");
        assert_eq!(d.format, FormatId::Yaml);
    }

    #[test]
    fn a_markdown_file_with_colons_is_not_yaml() {
        // The false positive that motivates the rule.
        let md = "# Options\n\nname: the name to use\nversion: which version\n";
        let d = sniff_heuristic(md).unwrap();
        assert_eq!(d.format, FormatId::Markdown, "the heading must win");
    }

    #[test]
    fn a_dash_rule_further_down_is_not_a_yaml_marker() {
        // In Markdown `---` is a horizontal rule. Only a marker on the first content line
        // counts as a YAML document start.
        let md = "Some text\n\n---\n\nmore text\n";
        let d = sniff_heuristic(md);
        assert!(
            d.is_none() || d.unwrap().format != FormatId::Yaml,
            "a mid-document --- is not YAML"
        );
    }

    #[test]
    fn detects_logs_by_timestamps() {
        let log = "2026-08-10T12:00:00Z INFO  starting\n\
                   2026-08-10T12:00:01Z INFO  connected\n\
                   2026-08-10T12:00:02Z WARN  retrying\n\
                   2026-08-10T12:00:03Z ERROR gave up\n";
        let d = sniff_heuristic(log).unwrap();
        assert_eq!(d.format, FormatId::Log);
    }

    #[test]
    fn recognizes_syslog_and_bare_clock_shapes() {
        let syslog = "Aug 10 12:00:00 host daemon: up\n\
                      Aug 10 12:00:01 host daemon: ok\n\
                      Aug 10 12:00:02 host daemon: fine\n";
        assert_eq!(sniff_heuristic(syslog).unwrap().format, FormatId::Log);

        let clock = "12:00:00 one\n12:00:01 two\n12:00:02 three\n";
        assert_eq!(sniff_heuristic(clock).unwrap().format, FormatId::Log);
    }

    #[test]
    fn one_timestamped_line_is_not_a_log() {
        let text = "2026-08-10 was a good day\n\
                    but this line is prose\n\
                    and so is this one\n\
                    and this one too\n";
        let d = sniff_heuristic(text);
        assert!(
            d.is_none() || d.unwrap().format != FormatId::Log,
            "a log needs most lines timestamped"
        );
    }

    // ---------------- robustness

    #[test]
    fn nothing_panics_on_awkward_input() {
        for input in [
            "",
            " ",
            "\n\n\n",
            "<",
            "{",
            "[",
            "#",
            "---",
            "\"",
            "{\"a\":",
            "|||",
            ",,,",
            "\u{1f600}",
            "日本語のテキスト",
        ] {
            let _ = sniff(input);
            let _ = sniff_heuristic(input);
        }
    }
}
