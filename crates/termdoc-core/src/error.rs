//! Errors and exit codes.
//!
//! The codes are fixed in docs/DESIGN.md §7 and verified by the CLI tests.

use std::path::PathBuf;

/// Process exit codes. `OK` is 0 even when warnings were emitted: a document rendered
/// with one questionable table is still a success.
pub mod exit {
    pub const OK: i32 = 0;
    pub const GENERIC: i32 = 1;
    pub const USAGE: i32 = 2;
    pub const UNSUPPORTED: i32 = 3;
    pub const UNREADABLE: i32 = 4;
    pub const PLUGIN: i32 = 5;
    pub const INTERRUPTED: i32 = 130;
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot read '{path}': {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{0}")]
    Stdio(#[from] std::io::Error),

    #[error("unsupported format: {0}")]
    Unsupported(String),

    #[error("corrupt document: {0}")]
    Corrupt(String),

    #[error("invalid encoding: {0}")]
    Encoding(String),

    #[error("{0}")]
    Usage(String),

    #[error("plugin '{name}' failed: {message}")]
    Plugin { name: String, message: String },
}

impl Error {
    /// The associated exit code. The single place where this is decided, so the CLI does
    /// not end up with duplicated `match` arms that drift apart.
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Usage(_) => exit::USAGE,
            Error::Unsupported(_) => exit::UNSUPPORTED,
            Error::Io { .. } | Error::Corrupt(_) | Error::Encoding(_) => exit::UNREADABLE,
            Error::Plugin { .. } => exit::PLUGIN,
            Error::Stdio(e) if e.kind() == std::io::ErrorKind::BrokenPipe => exit::OK,
            Error::Stdio(_) => exit::GENERIC,
        }
    }

    /// `true` when the error is simply "the consumer closed the pipe".
    ///
    /// That is not a failure: `termdoc doc.md | head -5` must finish silently.
    pub fn is_broken_pipe(&self) -> bool {
        matches!(self, Error::Stdio(e) if e.kind() == std::io::ErrorKind::BrokenPipe)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broken_pipe_is_not_a_failure() {
        let e = Error::Stdio(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        assert!(e.is_broken_pipe());
        assert_eq!(e.exit_code(), exit::OK);
    }

    #[test]
    fn exit_codes_match_the_design() {
        assert_eq!(Error::Usage("x".into()).exit_code(), 2);
        assert_eq!(Error::Unsupported("x".into()).exit_code(), 3);
        assert_eq!(Error::Corrupt("x".into()).exit_code(), 4);
        assert_eq!(
            Error::Plugin {
                name: "x".into(),
                message: "y".into()
            }
            .exit_code(),
            5
        );
    }
}
