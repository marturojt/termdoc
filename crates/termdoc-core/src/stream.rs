//! Input that arrives over time: a pipe, a terminal, `kubectl logs -f`.
//!
//! A file is a slice of bytes that is all there, so a reader can borrow from it for as long as
//! it likes. A pipe is not: its end may be minutes away or never come, and a reader that waits
//! for the end (as `Source::from_stdin` does) shows nothing until then — and, for `yes | termdoc`,
//! never shows anything at all. This module is the other half.
//!
//! # How it works
//!
//! A thread reads stdin in chunks and hands them over a small bounded queue. Two things follow
//! from that and are the reason for it:
//!
//! - **Detection can look before committing.** [`StdinFeed::fill_probe`] waits for the first data
//!   and then a short grace period for more, so the format is decided from what has arrived
//!   instead of from nothing, and a slow stream is never held up waiting for a full probe.
//! - **The consumer can tell, before it blocks, that it is about to.** [`StreamWatch::starved`]
//!   is true when the next read will wait, which is exactly when output has to be flushed: a
//!   line that sits in a buffer while the program waits for the next one is a log that appears
//!   to hang. Flushing after every line would cost a system call per line on a `cat huge.log`,
//!   so it is done only when the queue is empty.
//!
//! The queue is bounded, so a fast producer waits for a slow consumer (`yes | termdoc | head`
//! does not buffer the world), and nothing already shown is retained.
//!
//! # What it deliberately does not do
//!
//! The encoding is decided from the first chunk, as it was from the first kilobytes of a file.
//! A stream that changes encoding halfway gets replacement characters and one warning.

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError, sync_channel};
use std::time::Duration;

use crate::{Error, PROBE_SIZE, Result, Source};

/// What a stream decodes with, re-exported so a reader needs no dependency of its own for it.
pub use encoding_rs::{Encoding, UTF_8};

/// One read from the producer. Large enough to be cheap, small enough to keep latency low.
const CHUNK: usize = 64 * 1024;

/// How many chunks may wait for the consumer before the producer is made to wait too.
const QUEUE: usize = 8;

/// How long [`StdinFeed::fill_probe`] waits for more data once some has arrived.
pub const PROBE_GRACE: Duration = Duration::from_millis(100);

/// A line longer than this is split rather than buffered without bound.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// What the consumer and the observer share.
#[derive(Debug, Default)]
struct Shared {
    /// Chunks sent by the reader thread and not yet taken.
    queued: AtomicUsize,
    /// A complete line is waiting in the consumer's own buffer.
    buffered: AtomicBool,
    eof: AtomicBool,
}

/// Tells an observer, without blocking, whether the stream is about to make its consumer wait.
#[derive(Clone, Debug)]
pub struct StreamWatch(Arc<Shared>);

impl StreamWatch {
    /// `true` when the next line is not available yet: nothing complete is buffered, nothing is
    /// queued, and the input has not ended. This is the moment to flush output.
    pub fn starved(&self) -> bool {
        !self.0.buffered.load(Ordering::Acquire)
            && self.0.queued.load(Ordering::Acquire) == 0
            && !self.0.eof.load(Ordering::Acquire)
    }
}

enum Take {
    Block,
    Timeout(Duration),
}

enum Got {
    Data,
    Empty,
    Ended,
}

/// The reading end of an input that may still be arriving.
#[derive(Debug)]
pub struct StdinFeed {
    rx: Receiver<std::io::Result<Vec<u8>>>,
    /// Everything received and not yet consumed.
    carry: Vec<u8>,
    eof: bool,
    error: Option<std::io::Error>,
    shared: Arc<Shared>,
}

impl StdinFeed {
    /// Starts reading standard input.
    pub fn stdin() -> Self {
        Self::from_reader(std::io::stdin())
    }

    /// Starts reading any `Read`. Standard input goes through here; so do the tests.
    pub fn from_reader(mut reader: impl Read + Send + 'static) -> Self {
        let (tx, rx) = sync_channel::<std::io::Result<Vec<u8>>>(QUEUE);
        let shared = Arc::new(Shared::default());
        let queued = Arc::clone(&shared);
        std::thread::spawn(move || {
            loop {
                let mut chunk = vec![0u8; CHUNK];
                match reader.read(&mut chunk) {
                    // The sender is dropped here, which is how the other side learns of EOF.
                    Ok(0) => break,
                    Ok(n) => {
                        chunk.truncate(n);
                        queued.queued.fetch_add(1, Ordering::AcqRel);
                        if tx.send(Ok(chunk)).is_err() {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        break;
                    }
                }
            }
        });
        StdinFeed {
            rx,
            carry: Vec::new(),
            eof: false,
            error: None,
            shared,
        }
    }

    fn take(&mut self, how: Take) -> Got {
        let item = match how {
            Take::Block => self.rx.recv().map_err(|_| TryRecvError::Disconnected),
            Take::Timeout(d) => self.rx.recv_timeout(d).map_err(|e| match e {
                RecvTimeoutError::Timeout => TryRecvError::Empty,
                RecvTimeoutError::Disconnected => TryRecvError::Disconnected,
            }),
        };
        match item {
            Ok(Ok(chunk)) => {
                self.shared.queued.fetch_sub(1, Ordering::AcqRel);
                self.carry.extend_from_slice(&chunk);
                Got::Data
            }
            Ok(Err(e)) => {
                self.error = Some(e);
                self.end();
                Got::Ended
            }
            Err(TryRecvError::Empty) => Got::Empty,
            Err(TryRecvError::Disconnected) => {
                self.end();
                Got::Ended
            }
        }
    }

    fn end(&mut self) {
        self.eof = true;
        self.shared.eof.store(true, Ordering::Release);
    }

    /// Waits for the first data, then up to `grace` for more, until `want` bytes are in hand.
    ///
    /// A slow stream is not held up: once something has arrived, silence for `grace` ends the
    /// wait. Detection works from whatever this gathered.
    pub fn fill_probe(&mut self, want: usize, grace: Duration) {
        while self.carry.len() < want && !self.eof {
            let how = if self.carry.is_empty() {
                Take::Block
            } else {
                Take::Timeout(grace)
            };
            match self.take(how) {
                Got::Data => {}
                Got::Empty | Got::Ended => break,
            }
        }
    }

    /// [`fill_probe`](Self::fill_probe) with the usual size and grace period.
    pub fn probe(&mut self) {
        self.fill_probe(PROBE_SIZE, PROBE_GRACE);
    }

    /// What has arrived and has not been consumed.
    pub fn buffered(&self) -> &[u8] {
        &self.carry
    }

    /// Whether the input has ended and everything has been received.
    pub fn is_finished(&self) -> bool {
        self.eof
    }

    /// A `Source` holding what has arrived, for detection to look at. The input is untouched.
    pub fn peek_source(&self) -> Source {
        Source::from_stdin_bytes(self.carry.clone())
    }

    /// Reads to the end and returns everything as one `Source`: the path for formats that need
    /// the whole document (Markdown, JSON, a table), which is what stdin always was.
    pub fn into_source(mut self) -> Result<Source> {
        while !self.eof {
            self.take(Take::Block);
        }
        if let Some(e) = self.error.take() {
            return Err(Error::from(e));
        }
        Ok(Source::from_stdin_bytes(self.carry))
    }

    /// Turns the feed into a stream of decoded lines.
    pub fn into_lines(self, encoding: &'static encoding_rs::Encoding) -> LineStream {
        LineStream {
            feed: self,
            start: 0,
            encoding,
            offset: 0,
            number: 0,
            max_line: MAX_LINE_BYTES,
        }
    }

    /// An observer for [`StreamWatch::starved`].
    pub fn watch(&self) -> StreamWatch {
        StreamWatch(Arc::clone(&self.shared))
    }
}

/// One line of a stream, decoded, with the terminator it arrived with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamLine {
    pub text: String,
    /// The bytes were not valid in the stream's encoding and were replaced.
    pub had_errors: bool,
    /// Byte offsets of the line in the input.
    pub start: u64,
    pub end: u64,
    /// 1-based.
    pub number: u32,
}

/// An input read a line at a time as it arrives.
#[derive(Debug)]
pub struct LineStream {
    feed: StdinFeed,
    /// Where the unconsumed part of `feed.carry` begins.
    start: usize,
    encoding: &'static encoding_rs::Encoding,
    offset: u64,
    number: u32,
    max_line: usize,
}

impl LineStream {
    /// An observer for [`StreamWatch::starved`].
    pub fn watch(&self) -> StreamWatch {
        self.feed.watch()
    }

    /// A different line-length limit, for tests that should not need sixteen megabytes.
    pub fn with_max_line(mut self, bytes: usize) -> Self {
        self.max_line = bytes;
        self
    }

    /// The name of the encoding lines are decoded with.
    pub fn encoding_name(&self) -> &'static str {
        self.encoding.name()
    }

    /// The I/O error that ended the input, if one did.
    pub fn take_error(&mut self) -> Option<std::io::Error> {
        self.feed.error.take()
    }

    /// The next line, waiting for it if it has not arrived. `None` at the end of the input.
    pub fn next_line(&mut self) -> Option<StreamLine> {
        let shared = Arc::clone(&self.feed.shared);
        loop {
            let avail = &self.feed.carry[self.start..];
            let len = if let Some(nl) = avail.iter().position(|b| *b == b'\n') {
                Some(nl + 1)
            } else if avail.len() >= self.max_line {
                // No newline in sight and no room for more: split, on a character boundary.
                let mut cut = self.max_line;
                while cut > 1 && (avail[cut] & 0xC0) == 0x80 {
                    cut -= 1;
                }
                Some(cut)
            } else if self.feed.eof && !avail.is_empty() {
                Some(avail.len())
            } else {
                None
            };

            if let Some(len) = len {
                let bytes = &avail[..len];
                let (text, had_errors) = self.decode(bytes);
                let start = self.offset;
                self.offset += len as u64;
                self.number += 1;
                self.start += len;
                self.compact();
                shared.buffered.store(
                    self.feed.carry[self.start..].contains(&b'\n'),
                    Ordering::Release,
                );
                return Some(StreamLine {
                    text,
                    had_errors,
                    start,
                    end: self.offset,
                    number: self.number,
                });
            }
            if self.feed.eof {
                return None;
            }
            // About to wait: nothing complete is buffered.
            shared.buffered.store(false, Ordering::Release);
            self.feed.take(Take::Block);
        }
    }

    fn decode(&self, bytes: &[u8]) -> (String, bool) {
        if self.encoding == encoding_rs::UTF_8
            && let Ok(s) = std::str::from_utf8(bytes)
        {
            return (s.to_string(), false);
        }
        // Without BOM handling: the encoding was decided by detection, and a BOM check on one
        // line out of many would be meaningless.
        let (text, errors) = self.encoding.decode_without_bom_handling(bytes);
        (text.into_owned(), errors)
    }

    /// Drops what has been consumed, so a stream that runs for a week does not keep a week.
    fn compact(&mut self) {
        if self.start == self.feed.carry.len() {
            self.feed.carry.clear();
            self.start = 0;
        } else if self.start >= CHUNK {
            self.feed.carry.drain(..self.start);
            self.start = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::mpsc::{Sender, channel};
    use std::time::Instant;

    /// A reader whose data arrives when the test says so.
    struct Trickle(Receiver<Vec<u8>>);

    impl Read for Trickle {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.recv() {
                Ok(data) => {
                    buf[..data.len()].copy_from_slice(&data);
                    Ok(data.len())
                }
                Err(_) => Ok(0),
            }
        }
    }

    fn trickle() -> (Sender<Vec<u8>>, StdinFeed) {
        let (tx, rx) = channel();
        (tx, StdinFeed::from_reader(Trickle(rx)))
    }

    fn lines(input: &[u8]) -> Vec<StreamLine> {
        let mut s =
            StdinFeed::from_reader(Cursor::new(input.to_vec())).into_lines(encoding_rs::UTF_8);
        std::iter::from_fn(|| s.next_line()).collect()
    }

    fn texts(input: &[u8]) -> Vec<String> {
        lines(input).into_iter().map(|l| l.text).collect()
    }

    #[test]
    fn lines_keep_their_terminators_and_the_last_may_have_none() {
        assert_eq!(texts(b"one\ntwo\r\nthree"), ["one\n", "two\r\n", "three"]);
        assert_eq!(texts(b"a\n"), ["a\n"]);
        assert_eq!(texts(b"\n\n"), ["\n", "\n"]);
        assert!(texts(b"").is_empty());
    }

    #[test]
    fn offsets_and_numbers_follow_the_input() {
        let l = lines(b"ab\ncd\nef");
        assert_eq!(
            l.iter()
                .map(|l| (l.number, l.start, l.end))
                .collect::<Vec<_>>(),
            [(1, 0, 3), (2, 3, 6), (3, 6, 8)]
        );
    }

    #[test]
    fn a_line_split_across_chunks_is_one_line() {
        let (tx, feed) = trickle();
        let mut s = feed.into_lines(encoding_rs::UTF_8);
        tx.send(b"hel".to_vec()).unwrap();
        tx.send(b"lo wor".to_vec()).unwrap();
        tx.send(b"ld\nnext\n".to_vec()).unwrap();
        drop(tx);
        assert_eq!(s.next_line().unwrap().text, "hello world\n");
        assert_eq!(s.next_line().unwrap().text, "next\n");
        assert!(s.next_line().is_none());
    }

    #[test]
    fn the_stream_is_decoded_with_the_encoding_it_was_given() {
        let mut s = StdinFeed::from_reader(Cursor::new(b"Comit\xE9\n".to_vec()))
            .into_lines(encoding_rs::WINDOWS_1252);
        let l = s.next_line().unwrap();
        assert_eq!(l.text, "Comité\n");
        assert!(!l.had_errors);
    }

    #[test]
    fn invalid_bytes_are_replaced_and_flagged() {
        let mut s = StdinFeed::from_reader(Cursor::new(b"a\xFFb\n".to_vec()))
            .into_lines(encoding_rs::UTF_8);
        let l = s.next_line().unwrap();
        assert!(l.text.contains('\u{FFFD}'));
        assert!(l.had_errors);
    }

    #[test]
    fn a_line_with_no_end_in_sight_is_split_rather_than_buffered_forever() {
        let mut s = StdinFeed::from_reader(Cursor::new("é".repeat(100).into_bytes()))
            .into_lines(encoding_rs::UTF_8)
            .with_max_line(51);
        let first = s.next_line().unwrap();
        // 51 would cut an `é` in half; the cut backs off to a boundary.
        assert!(first.text.len() <= 51);
        assert!(!first.had_errors, "{:?}", first.text);
        let rest: String = std::iter::from_fn(|| s.next_line())
            .map(|l| l.text)
            .collect();
        assert_eq!(first.text + &rest, "é".repeat(100));
    }

    #[test]
    fn the_probe_gathers_what_arrives_and_does_not_wait_for_more_than_the_grace() {
        let (tx, mut feed) = trickle();
        tx.send(b"2026-08-10 INFO first\n".to_vec()).unwrap();
        let t = Instant::now();
        feed.fill_probe(PROBE_SIZE, Duration::from_millis(50));
        // One short chunk, then silence: it must not wait for 8 KiB that will never come.
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        assert_eq!(feed.buffered(), b"2026-08-10 INFO first\n");
        assert!(!feed.is_finished());
        drop(tx);
    }

    #[test]
    fn the_probe_of_an_empty_input_ends_with_the_input() {
        let mut feed = StdinFeed::from_reader(Cursor::new(Vec::new()));
        feed.probe();
        assert!(feed.is_finished());
        assert!(feed.buffered().is_empty());
    }

    #[test]
    fn the_peek_leaves_the_input_for_the_reader() {
        let mut feed = StdinFeed::from_reader(Cursor::new(b"hello\nworld\n".to_vec()));
        feed.probe();
        assert_eq!(feed.peek_source().bytes(), b"hello\nworld\n");
        let all = feed.into_source().unwrap();
        assert_eq!(all.bytes(), b"hello\nworld\n");
    }

    #[test]
    fn into_source_waits_for_the_whole_input() {
        let (tx, feed) = trickle();
        tx.send(b"part one, ".to_vec()).unwrap();
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            tx.send(b"part two".to_vec()).unwrap();
        });
        assert_eq!(feed.into_source().unwrap().bytes(), b"part one, part two");
        sender.join().unwrap();
    }

    /// Polls until `cond` holds, because the reader thread hands data over asynchronously.
    fn eventually(what: &str, cond: impl Fn() -> bool) {
        let t = Instant::now();
        while !cond() {
            assert!(t.elapsed() < Duration::from_secs(5), "never: {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn it_reports_starvation_exactly_when_the_next_read_would_wait() {
        let (tx, feed) = trickle();
        let mut s = feed.into_lines(encoding_rs::UTF_8);
        let watch = s.watch();

        tx.send(b"a\nb\n".to_vec()).unwrap();
        assert_eq!(s.next_line().unwrap().text, "a\n");
        // `b` is already here: the next read does not wait, so there is nothing to flush for.
        assert!(!watch.starved());
        assert_eq!(s.next_line().unwrap().text, "b\n");
        // Everything has been handed on and nothing is coming: the next read waits.
        eventually("starved after draining", || watch.starved());

        // More arrives: no longer starved, and the line is there without waiting.
        tx.send(b"c\n".to_vec()).unwrap();
        eventually("not starved once data is queued", || !watch.starved());
        assert_eq!(s.next_line().unwrap().text, "c\n");

        // The end of the input is not starvation: there is nothing left to wait for.
        eventually("starved again", || watch.starved());
        drop(tx);
        assert!(s.next_line().is_none());
        assert!(!watch.starved());
    }

    #[test]
    fn a_partial_line_is_starvation_too() {
        let (tx, feed) = trickle();
        let mut s = feed.into_lines(encoding_rs::UTF_8);
        let watch = s.watch();
        tx.send(b"whole\npartial".to_vec()).unwrap();
        assert_eq!(s.next_line().unwrap().text, "whole\n");
        // `partial` has no newline, so nothing complete is buffered; the next read waits.
        eventually("starved with only a partial line", || watch.starved());
        tx.send(b" line\n".to_vec()).unwrap();
        assert_eq!(s.next_line().unwrap().text, "partial line\n");
    }

    #[test]
    fn a_slow_producer_is_held_to_the_pace_of_the_consumer() {
        // The queue is bounded: this would use unbounded memory if it were not.
        let chunk = vec![b'x'; CHUNK - 1];
        let mut input = Vec::new();
        for _ in 0..(QUEUE * 4) {
            input.extend_from_slice(&chunk);
            input.push(b'\n');
        }
        let feed = StdinFeed::from_reader(Cursor::new(input));
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            feed.shared.queued.load(Ordering::Acquire) <= QUEUE + 1,
            "{} chunks queued",
            feed.shared.queued.load(Ordering::Acquire)
        );
        let mut s = feed.into_lines(encoding_rs::UTF_8);
        assert_eq!(std::iter::from_fn(|| s.next_line()).count(), QUEUE * 4);
    }
}
