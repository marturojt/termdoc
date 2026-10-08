//! Recognising a timestamp at the start of a line, without a regex engine.
//!
//! Two layers need this and must agree: detection decides that a file *is* a log because its
//! lines begin with timestamps, and the log reader colours the timestamp it found. They share
//! this one parser (docs/HANDOFF.md M1-4) so that "looks like a log" and "highlighted like a
//! log" cannot drift apart. It lives in `core` because a reader may depend on nothing else.
//!
//! The shapes that actually turn up:
//!
//! | shape | example |
//! |---|---|
//! | ISO-8601, `T` or a space, fraction and zone optional | `2026-08-10T12:00:00.123Z`, `2026-08-10 12:00:00,5 +02:00` |
//! | the same with slashes, or a bare date | `2026/08/10 12:00:00`, `2026-08-10` |
//! | a bare clock | `12:00:00.250` |
//! | syslog | `Aug  9 12:00:00` |
//! | any of those in brackets | `[2026-08-10 12:00:00]` |

use std::ops::Range;

/// The timestamp a line starts with, as a byte range of the line.
///
/// For a bracketed one the range is the *inside*: the brackets are punctuation, not part of the
/// time. Leading blanks are skipped.
pub fn leading_timestamp(line: &str) -> Option<Range<usize>> {
    let b = line.as_bytes();
    let start = b.iter().take_while(|c| **c == b' ' || **c == b'\t').count();

    if b.get(start) == Some(&b'[') {
        let inner = start + 1;
        let len = scan(&b[inner..])?;
        // A bracket that never closes is a different kind of line (`[INFO] ...` has no time).
        return (b.get(inner + len) == Some(&b']')).then_some(inner..inner + len);
    }
    let len = scan(&b[start..])?;
    Some(start..start + len)
}

fn digits(b: &[u8], n: usize) -> bool {
    b.len() >= n && b[..n].iter().all(u8::is_ascii_digit)
}

/// `hh:mm:ss`, with an optional fraction, at the start of `b`: its length.
fn clock(b: &[u8]) -> Option<usize> {
    if !(digits(b, 2) && b.get(2) == Some(&b':') && digits(&b[3..], 2)) {
        return None;
    }
    if !(b.get(5) == Some(&b':') && digits(&b[6..], 2)) {
        return None;
    }
    let mut end = 8;
    // `12:00:00.123` or `12:00:00,123`
    if matches!(b.get(end), Some(b'.' | b',')) && b.get(end + 1).is_some_and(u8::is_ascii_digit) {
        end += 1;
        while b.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
    }
    Some(end)
}

/// A UTC marker or an offset (`Z`, `+02`, `+0200`, `-05:00`) at the start of `b`: its length.
fn zone(b: &[u8]) -> usize {
    match b.first() {
        Some(b'Z') => 1,
        Some(b'+' | b'-') if digits(&b[1..], 2) => {
            let mut end = 3;
            if b.get(end) == Some(&b':') && digits(&b[end + 1..], 2) {
                end += 3;
            } else if digits(&b[end..], 2) {
                end += 2;
            }
            end
        }
        _ => 0,
    }
}

fn scan(b: &[u8]) -> Option<usize> {
    // `2026-08-10` or `2026/08/10`, then optionally a time.
    if digits(b, 4)
        && matches!(b.get(4), Some(b'-' | b'/'))
        && digits(&b[5..], 2)
        && b.get(7) == b.get(4)
        && digits(&b[8..], 2)
    {
        let mut end = 10;
        if matches!(b.get(end), Some(b'T' | b' '))
            && let Some(time) = clock(&b[end + 1..])
        {
            end += 1 + time;
            // A space before the zone is how some loggers write it: `12:00:00 +0200`.
            let skip = usize::from(b.get(end) == Some(&b' '));
            let z = zone(&b[(end + skip).min(b.len())..]);
            if z > 0 {
                end += skip + z;
            }
        }
        return Some(end);
    }

    if let Some(len) = clock(b) {
        return Some(len);
    }

    // Syslog: `Aug 10 12:00:00`, with a padded day (`Aug  9`) allowed.
    const MONTHS: [&[u8]; 12] = [
        b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov",
        b"Dec",
    ];
    if b.len() >= 15 && MONTHS.contains(&&b[..3]) && b[3] == b' ' {
        let mut i = 4;
        if b[i] == b' ' {
            i += 1;
        }
        let day = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        if (1..=2).contains(&day) && b.get(i + day) == Some(&b' ') {
            let time = clock(&b[i + day + 1..])?;
            return Some(i + day + 1 + time);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(line: &str) -> Option<&str> {
        leading_timestamp(line).map(|r| &line[r])
    }

    #[test]
    fn iso_8601_in_its_usual_forms() {
        assert_eq!(found("2026-08-10 INFO x"), Some("2026-08-10"));
        assert_eq!(found("2026-08-10T12:00:00 x"), Some("2026-08-10T12:00:00"));
        assert_eq!(
            found("2026-08-10T12:00:00Z x"),
            Some("2026-08-10T12:00:00Z")
        );
        assert_eq!(
            found("2026-08-10T12:00:00.123456+02:00 x"),
            Some("2026-08-10T12:00:00.123456+02:00")
        );
        assert_eq!(
            found("2026-08-10 12:00:00,123 INFO"),
            Some("2026-08-10 12:00:00,123")
        );
        assert_eq!(found("2026/08/10 12:00:00 x"), Some("2026/08/10 12:00:00"));
        assert_eq!(
            found("2026-08-10 12:00:00 +0200 x"),
            Some("2026-08-10 12:00:00 +0200")
        );
    }

    #[test]
    fn the_message_is_not_swallowed_into_the_zone() {
        // A `-` after the time is the message's, not an offset.
        assert_eq!(
            found("2026-08-10 12:00:00 - started"),
            Some("2026-08-10 12:00:00")
        );
        assert_eq!(found("2026-08-10 12:00:00 +x"), Some("2026-08-10 12:00:00"));
    }

    #[test]
    fn a_bare_clock_and_syslog() {
        assert_eq!(found("12:00:00 started"), Some("12:00:00"));
        assert_eq!(found("12:00:00.250 started"), Some("12:00:00.250"));
        assert_eq!(found("Aug 10 12:00:00 host sshd"), Some("Aug 10 12:00:00"));
        assert_eq!(found("Aug  9 12:00:00 host"), Some("Aug  9 12:00:00"));
    }

    #[test]
    fn brackets_are_punctuation_and_blanks_are_skipped() {
        assert_eq!(
            found("[2026-08-10 12:00:00] x"),
            Some("2026-08-10 12:00:00")
        );
        assert_eq!(found("  2026-08-10 x"), Some("2026-08-10"));
        assert_eq!(leading_timestamp("  [12:00:00] x"), Some(3..11));
    }

    #[test]
    fn things_that_are_not_timestamps() {
        for line in [
            "",
            "hello",
            "[INFO] started",
            "[2026-08-10 12:00:00 never closed",
            "2026-8-10 x",
            "2026-08-1",
            "20260810",
            "12:00 x",
            "12-00-00 x",
            "Aug 10 x",
            "Augustus 10 12:00:00",
            "version 2026-08-10",
        ] {
            assert_eq!(found(line), None, "{line:?}");
        }
    }

    #[test]
    fn it_never_slices_inside_a_character() {
        // The ranges feed string slicing; a multi-byte character right after must be safe.
        for line in ["2026-08-10é", "12:00:00日本", "[12:00:00]日本"] {
            let r = leading_timestamp(line).unwrap();
            let _ = &line[r];
        }
    }
}
