# termdoc

A universal document viewer for the terminal.

[![crates.io](https://img.shields.io/crates/v/termdoc.svg)](https://crates.io/crates/termdoc)
[![CI](https://github.com/marturojt/termdoc/actions/workflows/ci.yml/badge.svg)](https://github.com/marturojt/termdoc/actions/workflows/ci.yml)

`termdoc` reads any document and renders it as well as the terminal allows. It never opens an
external application, and it degrades gracefully based on what the terminal can actually do.

It is not an editor. It is not a converter. It is not an IDE.

```bash
termdoc report.pdf
termdoc manual.docx
termdoc README.md
termdoc book.epub
termdoc data.csv

cat README.md | termdoc
curl -sL https://example.com/a.pdf | termdoc
git show HEAD:README.md | termdoc
```

When stdout is a terminal, it opens an interactive pager with search and table-of-contents
navigation. When it is a pipe or a redirection, it emits a clean, composable stream.

## Status

**M0 is complete.** It works with **Markdown**, **plain text** and **logs**.

Already implemented: the streaming document model, a layout engine with Unicode-aware wrapping
and tables, ANSI and plain-text backends, terminal capability detection, and a CLI that behaves
correctly in pipes.

Measured: **3.9 ms** startup, and **1.4 MB** of own memory while walking a 488 MB log.

**M1 is under way.** The layered format- and encoding-detection engine has landed, so `termdoc`
already recognizes JSON, YAML, TOML, XML, HTML, CSV, source code, PDF and the Office/ZIP family, and
decodes non-UTF-8 files correctly. The dedicated readers for those formats are next; until each one
lands, a recognized-but-unreadable text format is shown as plain text with a warning.

The full roadmap is in [`docs/DESIGN.md`](docs/DESIGN.md) §11.

## Install

```bash
cargo install termdoc
```

Or from source:

```bash
cargo build --release
./target/release/termdoc README.md
```

## Principles

- Do one thing and do it well.
- Near-instant startup.
- Low memory, even with enormous files.
- Highly extensible through plugins.
- Correct with pipes and scripts.
- Feels like a native system utility.
- Graceful degradation is a principle, not a detail.

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The architecture is documented in [`docs/DESIGN.md`](docs/DESIGN.md), the notes for working in this
repository are in [`CLAUDE.md`](CLAUDE.md), and [`docs/HANDOFF.md`](docs/HANDOFF.md) records where
the work stands and what comes next.

## License

MIT OR Apache-2.0
