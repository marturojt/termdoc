# 0008. `SIGPIPE` reset to `SIG_DFL`

**Status:** Accepted

## Context

Rust ignores `SIGPIPE`, so `termdoc huge.log | head -5` would end in a broken-pipe panic instead of dying quietly like `cat`.

## Decision

The first line of `main` restores `SIGPIPE` to its default. On Windows, where a closed pipe surfaces as a write error, `BrokenPipe` is treated as success.

## Consequences

Behaves like a Unix utility. It is the most important line in `main`, and `tests/integration.rs` and `perf-gate.py` both pin it.
