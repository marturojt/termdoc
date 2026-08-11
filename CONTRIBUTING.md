# Contributing

Issues and pull requests are welcome. The most useful contribution right now is a
**reader for one of the formats marked 🚧** in the [README](README.md#supported-formats):
detection already recognises them, so what is missing is the part that turns their
bytes into the document model.

## Before anything structural

Two documents outrank everything else, and reading them first will save you work:

- [`docs/DESIGN.md`](docs/DESIGN.md) — the architecture and the reasoning behind it.
  Section numbers are cited from code comments, so if you change a decision, `grep`
  for the section number to find everything that leans on it.
- [`docs/HANDOFF.md`](docs/HANDOFF.md) — where the work stands, what comes next, and
  the traps that have already cost someone a debugging session.

## Adding a document reader

The recipe is written down rather than folklore:
**[CLAUDE.md § Adding a reader](CLAUDE.md#adding-a-reader)**.

The shape of it is that a reader crate depends on `termdoc-core` and nothing else.
That is not a convention — it is enforced by Cargo's dependency graph, and
[`crates/termdoc-cli/tests/layering.rs`](crates/termdoc-cli/tests/layering.rs) fails
if a new edge appears. So you can write a JSON reader without knowing anything about
how colour degrades or how tables allocate width.

A reader **describes** a document; it does not decide how it looks. If you find
yourself wanting the terminal width or a colour inside a reader, the answer belongs
in the layout or the theme.

## Working rules

[`CLAUDE.md`](CLAUDE.md) is the practical guide: the commands, the invariants the
tests protect, the five levels of the test suite and which one fits what you are
doing, plus the mistakes that have already been made once.

The invariants are not stylistic preferences — each has a test behind it. A few
worth knowing before your first patch:

- `SIGPIPE` is reset to `SIG_DFL`, so `termdoc huge.log | head -5` dies like `cat`
  rather than panicking.
- In a pipe, stdout carries only the document. Diagnostics go to stderr.
- With colour off, not one escape byte is emitted.
- What comes in borrowed goes out borrowed — if a `Cow::Borrowed` becomes `Owned`
  along the way, memory stops being flat.

## Before opening a pull request

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release && python3 scripts/perf-gate.py
```

CI runs these on Linux, macOS and Windows. Two notes that come up often:

- **CI's clippy may be newer than your toolchain**, and `-D warnings` makes any new
  lint a hard failure, so a green local clippy is not a guarantee. Read the CI log
  rather than trying to reproduce it.
- **Snapshot diffs must be read, not accepted blindly.** They exist to catch
  "something changed"; accepting without looking defeats the point.

Code, comments, test names and user-facing messages are all in **English**.

## License

By contributing you agree that your work is licensed under MIT OR Apache-2.0, the
same terms as the project.
