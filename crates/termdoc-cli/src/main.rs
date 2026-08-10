//! `termdoc`'s entry point.

mod cli;
mod run;

use std::io::Write;

use clap::Parser;
use termdoc_core::exit;

/// Restores `SIGPIPE` to the system's default behavior.
///
/// Rust ignores it at startup, and that decision —reasonable for a library— turns any Rust
/// CLI into a bad Unix citizen: `termdoc huge.log | head -5` would not finish quietly when
/// the pipe closes, but with a broken-pipe panic and garbage on stderr.
///
/// Restoring `SIG_DFL` makes the process die with signal 13 exactly like `cat` does, which is
/// what the rest of the system expects. This is the first thing `main` does, before any write.
#[cfg(unix)]
fn restore_sigpipe() {
    // SAFETY: calling `signal` with `SIG_DFL` for `SIGPIPE` is async-signal-safe, and this
    // runs before any thread is spawned or anything is written.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[cfg(not(unix))]
fn restore_sigpipe() {
    // Windows has no SIGPIPE: a closed pipe surfaces as a write error, and the `BrokenPipe`
    // handling already covers that.
}

fn main() {
    restore_sigpipe();

    let cli = cli::Cli::parse();

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let stderr = std::io::stderr();
    let mut err = stderr.lock();

    let code = match run::run(&cli, &mut out, &mut err) {
        Ok(code) => {
            // The explicit flush matters: failing to drain the buffer is a real write
            // failure and must not pass as success.
            match out.flush() {
                Ok(()) => code,
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => exit::OK,
                Err(e) => {
                    let _ = writeln!(err, "termdoc: write error: {e}");
                    exit::GENERIC
                }
            }
        }
        Err(e) => {
            let _ = out.flush();
            if e.is_broken_pipe() {
                // The consumer closed the pipe: not a failure, and nothing to report.
                exit::OK
            } else {
                let _ = writeln!(err, "termdoc: {e}");
                e.exit_code()
            }
        }
    };

    std::process::exit(code);
}
