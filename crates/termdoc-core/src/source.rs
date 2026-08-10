//! Input sources: memory-mapped files and stdin.

use std::io::Read;
use std::path::{Path, PathBuf};

use memmap2::Mmap;

use crate::{Error, Result};

/// How many prefix bytes are available for detection without consuming the input.
/// 8 KiB comfortably covers any structural sniff (docs/DESIGN.md §4).
pub const PROBE_SIZE: usize = 8 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    File(PathBuf),
    Stdin,
    /// Synthetic input: tests and embedded uses.
    Memory(String),
}

enum Data {
    /// Mapped: the OS pages in only what gets rendered, and `Cow::Borrowed` borrows
    /// straight from here without copying.
    Mapped(Mmap),
    Owned(Vec<u8>),
}

pub struct Source {
    origin: Origin,
    data: Data,
    /// Transcoded text, populated only when the source was not valid UTF-8.
    ///
    /// It lives here rather than in the reader so that `as_str` can return a `&str` with
    /// the source's lifetime. A reader that needs the whole document —Markdown, for
    /// instance— cannot borrow from a local `String` of its own without becoming
    /// self-referential; borrowing from the source, which outlives it, works.
    text_cache: std::sync::OnceLock<String>,
}

impl Source {
    /// Opens a file by memory-mapping it.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = std::fs::File::open(path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let len = file
            .metadata()
            .map_err(|source| Error::Io {
                path: path.to_path_buf(),
                source,
            })?
            .len();

        // `Mmap::map` fails on zero length, and an empty file is legitimate input.
        let data = if len == 0 {
            Data::Owned(Vec::new())
        } else {
            // SAFETY: if another process truncates the file while we read it, access can
            // fail with SIGBUS. This is the same contract `bat`, `rg` and `less` accept,
            // and the price of not copying gigabytes.
            let map = unsafe { Mmap::map(&file) }.map_err(|source| Error::Io {
                path: path.to_path_buf(),
                source,
            })?;

            // Tell the kernel the access will be sequential. Almost everything termdoc
            // does is a front-to-back traversal, and with this hint the kernel reads
            // ahead and drops behind instead of keeping the whole file resident.
            //
            // The hint is an optimization, not a requirement: on systems that ignore it,
            // nothing changes, which is exactly what should happen.
            #[cfg(unix)]
            let _ = map.advise(memmap2::Advice::Sequential);

            Data::Mapped(map)
        };

        Ok(Source {
            origin: Origin::File(path.to_path_buf()),
            data,
            text_cache: std::sync::OnceLock::new(),
        })
    }

    /// Reads stdin to completion.
    ///
    /// KNOWN M0 LIMITATION: stdin is fully buffered. The M0 formats (plain text and
    /// Markdown) gain nothing from incremental streaming —Markdown needs the whole input
    /// anyway— and huge files have the file path, which *is* lazy. The incremental stdin
    /// reader arrives with the log reader in M1, where `kubectl logs -f | termdoc` makes
    /// it essential.
    pub fn from_stdin() -> Result<Self> {
        let mut buf = Vec::new();
        std::io::stdin().lock().read_to_end(&mut buf)?;
        Ok(Source {
            origin: Origin::Stdin,
            data: Data::Owned(buf),
            text_cache: std::sync::OnceLock::new(),
        })
    }

    pub fn from_bytes(name: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Source {
            origin: Origin::Memory(name.into()),
            data: Data::Owned(bytes.into()),
            text_cache: std::sync::OnceLock::new(),
        }
    }

    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    /// The file path, when the source is a file. Extension-based detection needs it;
    /// stdin has none, which is why content sniffing is the primary path rather than an
    /// optional extra.
    pub fn path(&self) -> Option<&Path> {
        match &self.origin {
            Origin::File(p) => Some(p.as_path()),
            _ => None,
        }
    }

    /// A human-readable name for headers and error messages.
    pub fn display_name(&self) -> &str {
        match &self.origin {
            Origin::File(p) => p.to_str().unwrap_or("<non-UTF-8 path>"),
            Origin::Stdin => "<stdin>",
            Origin::Memory(n) => n.as_str(),
        }
    }

    pub fn bytes(&self) -> &[u8] {
        match &self.data {
            Data::Mapped(m) => m,
            Data::Owned(v) => v,
        }
    }

    pub fn len(&self) -> usize {
        self.bytes().len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes().is_empty()
    }

    /// The detection prefix, without consuming anything.
    pub fn peek(&self, n: usize) -> &[u8] {
        let b = self.bytes();
        &b[..n.min(b.len())]
    }

    pub fn probe(&self) -> &[u8] {
        self.peek(PROBE_SIZE)
    }

    /// The source as text.
    ///
    /// Returns `(text, was_lossy)`. Valid UTF-8 borrows without copying. Otherwise it
    /// degrades to replacement-character conversion rather than giving up: a latin-1
    /// document should still be readable, with a warning. Real encoding detection
    /// (`chardetng` + `encoding_rs`) lands in M1.
    pub fn text(&self) -> (std::borrow::Cow<'_, str>, bool) {
        let bytes = self.bytes();
        match std::str::from_utf8(bytes) {
            Ok(s) => (std::borrow::Cow::Borrowed(s), false),
            Err(_) => (String::from_utf8_lossy(bytes), true),
        }
    }

    /// The source as a `&str` with the source's own lifetime.
    ///
    /// Returns `(text, was_lossy)`. Readers that cannot work line by line need this —
    /// Markdown needs the complete document — because a `&'a str` borrowed from the
    /// source can travel inside the events, while a `String` local to the reader cannot.
    ///
    /// Valid UTF-8 copies nothing. Otherwise the conversion is cached here once.
    /// **This walks the entire source**, so a reader that *can* go line by line must use
    /// `bytes()` instead: that is the difference between `termdoc huge.log | head -5`
    /// reading a few pages and reading the whole file.
    pub fn as_str(&self) -> (&str, bool) {
        match std::str::from_utf8(self.bytes()) {
            Ok(s) => (s, false),
            Err(_) => {
                let cached = self
                    .text_cache
                    .get_or_init(|| String::from_utf8_lossy(self.bytes()).into_owned());
                (cached.as_str(), true)
            }
        }
    }

    /// Binary heuristic: a NUL byte in the prefix. The same rule `grep` and `git` use,
    /// and enough to avoid dumping an executable into the terminal.
    pub fn looks_binary(&self) -> bool {
        self.probe().contains(&0)
    }
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Source")
            .field("origin", &self.origin)
            .field("len", &self.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_bytes_borrows_when_the_input_is_utf8() {
        let s = Source::from_bytes("t", "hello");
        let (text, lossy) = s.text();
        assert_eq!(text, "hello");
        assert!(!lossy);
        assert!(matches!(text, std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn invalid_utf8_degrades_instead_of_failing() {
        let s = Source::from_bytes("t", vec![0xff, 0xfe, b'a']);
        let (text, lossy) = s.text();
        assert!(lossy, "the loss must be reported");
        assert!(text.contains('a'), "and what is readable must still show");
    }

    #[test]
    fn peek_does_not_overrun_short_input() {
        let s = Source::from_bytes("t", "ab");
        assert_eq!(s.peek(100), b"ab");
        assert_eq!(s.probe(), b"ab");
    }

    #[test]
    fn an_empty_file_is_valid_input() {
        let dir = std::env::temp_dir().join("termdoc-test-empty");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("empty.txt");
        std::fs::write(&path, b"").unwrap();

        let s = Source::open(&path).expect("an empty file must not be an error");
        assert!(s.is_empty());
        assert_eq!(s.text().0, "");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn detects_binary_by_nul_byte() {
        assert!(Source::from_bytes("t", vec![b'a', 0, b'b']).looks_binary());
        assert!(!Source::from_bytes("t", "normal text").looks_binary());
    }
}
