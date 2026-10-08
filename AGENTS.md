# AGENTS.md

This file provides guidance to Codex (Codex.ai/code) when working with code in this repository.

`termdoc` is a universal document viewer for the terminal, written in Rust. It reads any document
and renders it as well as the terminal allows, degrading based on the terminal's real capabilities.
It is not an editor, not a converter, and not an IDE.

**Two documents outrank this one:**

- [`docs/DESIGN.md`](docs/DESIGN.md) — the architecture and the reasoning behind it. **Read it
  before changing anything structural.** Its section numbers are referenced throughout this file.
- [`docs/HANDOFF.md`](docs/HANDOFF.md) — where the work stands, what comes next, and the traps
  already paid for. **Read it first if you are picking this up mid-stream.**

Status: **M0 and M1 complete** — detection, encoding, and readers for Markdown, plain text, logs,
JSON, YAML, TOML, XML, CSV and source code, with stdin read as it arrives. **M2, the pager, is next.**
Roadmap in `docs/DESIGN.md` §11.

Published: all nine crates are on crates.io at `0.2.0` (`cargo install termdoc`), the release binaries
are on GitHub, `brew install marturojt/tap/termdoc` works, and the site is
[termdoc.app](https://termdoc.app), whose source lives in the separate `marturojt/termdoc-site`
repository. **`termdoc --formats` is the authoritative answer** to what this build can read; the
README's table must be kept honest against it.

Code, comments, test names and user-facing messages are all in **English**.

`AGENTS.md` is a copy of CLAUDE.md for Codex. Keep the two in sync: edit CLAUDE.md, then mirror the change.

## Commands

```bash
cargo test --workspace                          # 449 tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo build --release                           # binary at target/release/termdoc
cargo run -q -- corpus/basic.md                 # run against the corpus

# A single test, or one test file
cargo test -p termdoc-layout wrap::tests::breaks_at_word_boundaries
cargo test -p termdoc-detect delimited          # every delimiter test
cargo test -p termdoc --test integration
cargo test -p termdoc-read-text --test events

# Snapshots. cargo-insta is NOT installed here, so accept in bulk and review the diff by hand:
INSTA_UPDATE=always cargo test -p termdoc --test snapshots
git diff crates/termdoc-cli/tests/snapshots/    # <- actually read this before committing

# Pathological corpus (not versioned, because of its size)
./scripts/gen-corpus.sh

# Performance gates (startup, own memory, lazy output)
cargo build --release && python3 scripts/perf-gate.py

# The README's demo image, regenerated from a real run. Do this whenever the
# renderer's colours, glyphs or wrapping change, or the asset starts lying.
python3 scripts/gen-demo-image.py
```

CI (`.github/workflows/ci.yml`) runs the tests on Linux/macOS/Windows, plus `fmt`, `clippy`, each
layer compiled separately, and the performance gates. Watch a run with
`gh run watch <id> --exit-status`.

**CI's clippy may be newer than the local toolchain** (this machine has Homebrew Rust 1.96.1 and no
`rustup`), and `RUSTFLAGS: -D warnings` makes any new lint a hard failure. A green local `clippy` is
therefore not a guarantee. If lint fails in CI but passes locally, that is why — read the CI log with
`gh run view <id> --log-failed` instead of trying to reproduce it.

## Architecture

### The pipeline

```
Source ──▶ Detect ──▶ DocumentReader ──▶ Event stream ──▶ Layout ──▶ Backend ──▶ stdout
```

Every stage is a trait in `termdoc-core`, and the stages do not know about each other. The only
module that sees all of them at once is `crates/termdoc-cli/src/run.rs`, where they get wired.

### The three decisions to understand before touching anything

**1. The document model is an event stream, not a tree.**
`Event::Start(Tag)` / `End(TagKind)` plus leaf events, with `Cow<'a, str>` borrowing from the
`mmap`. That is why a 488 MB log is read with 1.4 MB of own memory. If you materialize a tree along
the way, that property is gone. Anything needing random access (TOC, table widths) is materialized
locally, never the whole document. (§2.2)

**2. The layering is enforced by Cargo's graph, not by discipline.**

```
termdoc-core       → (nothing)       vocabulary: Event, Line, traits, Source
termdoc-term       → (nothing)       terminal capabilities, Fidelity
termdoc-detect     → core            what a document IS, never how it looks
termdoc-layout     → core, term      wrapping, tables, lists, glyphs
termdoc-backend    → core, term      ANSI and Plain
termdoc-read-*     → core            core ONLY: a reader cannot see a backend
                                     (read-text, read-data, read-code)
termdoc-cli        → everything      the wiring
```

`crates/termdoc-cli/tests/layering.rs` reads the `Cargo.toml` files and fails if a new edge appears.
If you genuinely need one, update `docs/DESIGN.md` §3 and that test's `ALLOWED` table, and explain
why. (§3)

`Line` lives in `core` rather than `layout` on purpose: it is the layout→backend contract, just as
`Event` is the reader→layout contract. That is what keeps `termdoc-backend` from depending on the
layout engine.

**3. Degradation is an input, not a chain of `if`s.**
`termdoc_term::Fidelity` (color, unicode, graphics, hyperlinks) is fed into the layout and the
backend. Every rung is pinned by snapshots in `crates/termdoc-cli/tests/snapshots/`. Ladders
implemented so far: color truecolor→256→16→none; tables box-drawing→ASCII→TSV; links OSC 8→`[n]`
references; headings styled→ATX notation. (§5)

### Invariants the tests protect

Do not break these without changing the design first:

- **`SIGPIPE` is reset to `SIG_DFL`** on the first line of `main`. Without it,
  `termdoc huge.log | head -5` ends in a panic instead of dying with signal 13 like `cat`. It is
  M0's most important criterion (`tests/integration.rs`).
- **In a pipe, stdout carries only the document.** Warnings and diagnostics go to stderr.
- **With `ColorDepth::None`, not one escape byte is emitted**, even when the layout asks for color.
- **Every line ends with no style active**: no attribute survives a newline.
- **No line exceeds the width in display cells**, with two documented exceptions: an indivisible
  grapheme cluster wider than the line (cluster integrity wins — see `wrap.rs`'s header) and the
  tables' TSV rung, which gives up width to preserve the data.
- **No line ends with spaces.** It dirties diffs and copy-paste.
- **What comes in borrowed goes out borrowed.** If `Cow::Borrowed` turns into `Owned` along the way,
  memory stops being flat.
- **A reader never decides how bytes become text.** The encoding is resolved by `termdoc-detect` and
  applied to the `Source`; readers call `decode_line`.

### Where things live

| What you need to change | File |
|---|---|
| The document model | `crates/termdoc-core/src/event.rs` |
| Input, mmap, encoding application | `crates/termdoc-core/src/source.rs` |
| Traits and the registry | `crates/termdoc-core/src/{traits,registry}.rs` |
| Unicode wrapping | `crates/termdoc-layout/src/wrap.rs` |
| Table width allocation | `crates/termdoc-layout/src/table.rs` |
| The events→lines state machine | `crates/termdoc-layout/src/engine.rs` |
| Colors and styles per role | `crates/termdoc-layout/src/theme.rs` |
| Glyphs per Unicode level | `crates/termdoc-layout/src/glyphs.rs` |
| ANSI sequences and color degradation | `crates/termdoc-backend/src/` |
| Terminal detection | `crates/termdoc-term/src/lib.rs` |
| Format detection layers | `crates/termdoc-detect/src/lib.rs` |
| CSV delimiter sniffing | `crates/termdoc-detect/src/delimited.rs` |
| Encoding detection | `crates/termdoc-detect/src/charset.rs` |
| Syntax highlighting: scope → role rules, ceilings, costs | `crates/termdoc-read-code/src/engine.rs` |
| Shared line-by-line reader plumbing | `crates/termdoc-core/src/highlight.rs` |
| Input that arrives over time (stdin feed, line stream, flush-when-starved) | `crates/termdoc-core/src/stream.rs` |
| The one timestamp parser (detection and the log reader share it) | `crates/termdoc-core/src/timestamp.rs` |
| Log highlighting: timestamp, level, logfmt keys | `crates/termdoc-read-text/src/log.rs` |
| Magic bytes and intra-ZIP | `crates/termdoc-detect/src/magic.rs` |
| CLI flags | `crates/termdoc-cli/src/cli.rs` |
| Pipeline wiring | `crates/termdoc-cli/src/run.rs` |
| The README demo image | `scripts/gen-demo-image.py` → `docs/assets/demo.png` |
| Contributor entry point | `CONTRIBUTING.md` (points at *Adding a reader* below) |

### Testing strategy

Five levels, each isolating a different class of failure. Use the one that fits:

- **Event-stream goldens** (`read-text/tests/events.rs`): the `Event` sequence with no ANSI in the
  way. Separates a parsing bug from a painting bug.
- **Snapshots** (`cli/tests/snapshots.rs`): the document × width × fidelity matrix. They detect that
  something *changed*.
- **Property tests** (`layout/tests/invariants.rs`): the wrapping invariants with generated input
  (CJK, combining marks, ZWJ, flags).
- **Integration** (`cli/tests/integration.rs`): behavior as a system utility — SIGPIPE, exit codes,
  clean output in a pipe, encoding end to end.
- **Layering** (`cli/tests/layering.rs`): the dependency-graph edges.

When adding a format: event goldens first, snapshots after. "Something changed" (snapshot) and
"something was lost" (`no_width_loses_characters`) are different failures, and there is a test for
each.

Tests that touch the filesystem must use a **per-call unique directory**. Cargo runs tests in
parallel, and two of them sharing a fixture path means one can `open` the file in the instant the
other truncated it to zero bytes. That already happened once, and the failure looked like a
detection bug rather than a fixture race. See `scratch()` in `tests/integration.rs`.

## Adding a reader

1. A new `crates/termdoc-read-<x>/` crate depending on **only** `termdoc-core`.
2. Implement `DocumentReader`; return `Events<'a>` borrowing from `Source`.
3. Expose `pub fn register(&mut Registry)` and call it from `run.rs::build_registry`. **Register no
   detectors** — naming formats belongs to `termdoc-detect`.
4. Add the crate to the `ALLOWED` table in `tests/layering.rs` and to `docs/DESIGN.md` §3.
5. Event goldens, a corpus file, snapshots.
6. Remove the format from `readable_as_text` in `run.rs` once the fallback no longer applies.

A reader **describes** the document; it does not decide how it looks. If you find yourself needing
the terminal width or a color inside a reader, the answer belongs in the layout or the theme.

## Things that surprise people

- `Source::as_str()` walks the entire source (it validates or transcodes). A reader that can go line
  by line must use `decode_line`: that is the difference between `| head -5` reading a few pages and
  reading the whole file.
- `Source::set_encoding` takes `&mut self` on purpose, so it must be applied before any reader
  borrows the source. The borrow checker enforces an ordering a comment would only ask for.
- `encoding_rs::decode` does BOM sniffing and **can override the encoding you asked for** — `FF FE`
  is a UTF-16LE BOM. Use `decode_without_bom_handling`; BOM handling belongs to `termdoc-detect`.
- Inside `Preformatted`/`CodeBlock`, **text accumulates until a newline closes the line**. A reader
  must keep the `\n` on every line it emits (`"one\n"`, still a borrowed slice) and may split a line
  into `Tag::Token` runs for colour. Emitting `Text("one")` with no terminator merges it into the
  next line. The layout trims `\n` and `\r\n` itself (DESIGN §2.2).
- **stdin is not a `Source`** while it may still be arriving. `run.rs` reads a first chunk for
  detection, then either hands a `LineStream` to a reader that says `streams_input()` (plain text,
  logs) or reads to the end and builds a `Source` as before. `Source::from_stdin` is no longer on
  that path. Output is flushed when the stream is *starved*, not per line.
- A reader cannot call `termdoc-detect`, so what detection knows reaches it through
  `ReadContext` (today: `delimiter` for CSV, filled by `run.rs::csv_delimiter`). Add a field there
  rather than a Cargo edge.
- The layout **does not** merge adjacent segments of the same style: that would require
  concatenating strings and would lose the `Cow::Borrowed`. Avoiding redundant SGR is the backend's
  job — it tracks the current style as state.
- `pulldown-cmark` emits the table header as bare cells, with no `TableRow`. The engine opens the
  row when it receives `TableHead`.
- `pulldown-cmark` does not number list items: the reader does, in `Marker::Ordered`.
- Raw HTML inside Markdown is dropped silently. Dumping `<br>` as text would be worse; the M3 HTML
  reader is the one that knows how to interpret it.
- `infer` recognizes **textual** formats too (`text/xml`), so `magic.rs` hands anything `text/*`
  onward instead of claiming it as binary at confidence 90.
- Detection can name formats this build has no reader for. `run.rs::pick_reader` degrades to plain
  text with a warning instead of failing — `--strict` turns that warning into an error.
- The memory budget is measured in **the process's anonymous memory**, not RSS. With `mmap`, RSS
  tracks the file size through clean page-cache pages, which are not memory the process owns (§8).
