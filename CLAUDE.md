# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

`termdoc` is a universal document viewer for the terminal, written in Rust. It reads any
document and renders it as well as the terminal allows, degrading based on the terminal's real
capabilities. It is not an editor, not a converter, and not an IDE.

**The full design lives in [`docs/DESIGN.md`](docs/DESIGN.md) and is the source of truth.**
Read it before changing architecture. Status: **M0 complete**, **M1 in progress** — detection and
encoding handling have landed, the data readers have not. The milestone roadmap is in §11.

Code, comments, test names and user-facing messages are all in **English**.

## Commands

```bash
cargo test --workspace                          # 258 tests
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release                           # binary at target/release/termdoc
cargo run -q -- corpus/basic.md                 # run against the corpus

# A single test, or one test file
cargo test -p termdoc-layout wrap::tests::breaks_at_word_boundaries
cargo test -p termdoc-cli --test integration
cargo test -p termdoc-read-text --test events

# Snapshots: review and accept changes (needs cargo-insta)
cargo insta review
INSTA_UPDATE=always cargo test -p termdoc-cli --test snapshots   # accept in bulk

# Pathological corpus (not versioned, because of its size)
./scripts/gen-corpus.sh

# Performance gates (startup, own memory, lazy output)
cargo build --release && python3 scripts/perf-gate.py
```

CI (`.github/workflows/ci.yml`) runs the tests on Linux/macOS/Windows, plus `fmt`, `clippy`,
each layer compiled separately, and the performance gates. The CI thresholds are looser than the
design's because a shared machine is slower and an intermittent gate is worse than no gate; they
are still tight enough to catch a real regression, which would be an order of magnitude.

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
`mmap`. That is why a 488 MB log is read with 1.4 MB of own memory. If you materialize a tree
along the way, that property is gone. Anything needing random access (TOC, table widths) is
materialized locally, never the whole document.

**2. The layering is enforced by Cargo's graph, not by discipline.**

```
termdoc-core       → (nothing)       vocabulary: Event, Line, traits, Source
termdoc-term       → (nothing)       terminal capabilities, Fidelity
termdoc-detect     → core            what a document IS, never how it looks
termdoc-layout     → core, term      wrapping, tables, lists, glyphs
termdoc-backend    → core, term      ANSI and Plain
termdoc-read-*     → core            core ONLY: a reader cannot see a backend
termdoc-cli        → everything      the wiring
```

`crates/termdoc-cli/tests/layering.rs` reads the `Cargo.toml` files and fails if a new edge
appears. If you genuinely need one, update `docs/DESIGN.md` §3 and that test's `ALLOWED` table,
and explain why.

`Line` lives in `core` rather than `layout` on purpose: it is the layout→backend contract, just
as `Event` is the reader→layout contract. That is what keeps `termdoc-backend` from depending on
the layout engine.

**3. Degradation is an input, not a chain of `if`s.**
`termdoc_term::Fidelity` (color, unicode, graphics, hyperlinks) is fed into the layout and the
backend. Every rung is pinned down by snapshots in `crates/termdoc-cli/tests/snapshots/`.
Ladders implemented so far: color truecolor→256→16→none; tables box-drawing→ASCII→TSV; links
OSC 8→`[n]` references; headings styled→ATX notation.

### Invariants the tests protect

Do not break these without changing the design first:

- **`SIGPIPE` is reset to `SIG_DFL`** on the first line of `main`. Without it,
  `termdoc huge.log | head -5` ends in a panic instead of dying with signal 13 like `cat`. It is
  M0's most important criterion (`tests/integration.rs`).
- **In a pipe, stdout carries only the document.** Warnings and diagnostics go to stderr.
- **With `ColorDepth::None`, not one escape byte is emitted**, even when the layout asks for
  color.
- **Every line ends with no style active**: no attribute survives a newline.
- **No line exceeds the width in display cells**, with two documented exceptions: an indivisible
  grapheme cluster wider than the line (cluster integrity wins — see `wrap.rs`'s header) and the
  tables' TSV rung, which gives up width to preserve the data.
- **No line ends with spaces.** It dirties diffs and copy-paste.
- **What comes in borrowed goes out borrowed**: if `Cow::Borrowed` turns into `Owned` along the
  way, memory stops being flat.

### Where things live

| What you need to change | File |
|---|---|
| The document model | `crates/termdoc-core/src/event.rs` |
| Unicode wrapping | `crates/termdoc-layout/src/wrap.rs` |
| Table width allocation | `crates/termdoc-layout/src/table.rs` |
| The events→lines state machine | `crates/termdoc-layout/src/engine.rs` |
| Colors and styles per role | `crates/termdoc-layout/src/theme.rs` |
| Glyphs per Unicode level | `crates/termdoc-layout/src/glyphs.rs` |
| ANSI sequences and color degradation | `crates/termdoc-backend/src/` |
| Terminal detection | `crates/termdoc-term/src/lib.rs` |
| Format detection layers | `crates/termdoc-detect/src/` |
| CSV delimiter sniffing | `crates/termdoc-detect/src/delimited.rs` |
| Encoding detection | `crates/termdoc-detect/src/charset.rs` |
| CLI flags | `crates/termdoc-cli/src/cli.rs` |

### Testing strategy

Each level isolates a different class of failure; use the one that fits:

- **Event-stream golden tests** (`read-text/tests/events.rs`): the `Event` sequence with no ANSI
  in the way. Separates a parsing bug from a painting bug.
- **Snapshots** (`cli/tests/snapshots.rs`): the document × width × fidelity matrix. They detect
  that something *changed*.
- **Property tests** (`layout/tests/invariants.rs`): the wrapping invariants with generated
  input (CJK, combining marks, ZWJ, flags).
- **Integration** (`cli/tests/integration.rs`): behavior as a system utility — SIGPIPE, exit
  codes, clean output in a pipe.
- **Layering** (`cli/tests/layering.rs`): the dependency-graph edges.

When adding a format: event goldens first, snapshots after. Detecting that something *changed*
(snapshot) and that something was *lost* (`no_width_loses_characters`) are different failures,
and there is a test for each.

## Adding a reader

1. A new `crates/termdoc-read-<x>/` crate depending on **only** `termdoc-core`.
2. Implement `DocumentReader`; return `Events<'a>` borrowing from `Source`.
3. Implement `Detector` with the appropriate confidence level (`termdoc_core::confidence`).
4. Expose `pub fn register(&mut Registry)` and call it from `run.rs::build_registry`.
5. Add the crate to the `ALLOWED` table in `tests/layering.rs`.
6. Event goldens, corpus, and snapshots.

A reader **describes** the document; it does not decide how it looks. If you find yourself
needing the terminal width or a color inside a reader, the answer belongs in the layout or the
theme.

## Things that surprise people

- `Source::as_str()` walks the entire source (it validates UTF-8). A reader that can go line by
  line must use `bytes()`: that is the difference between `| head -5` reading a few pages and
  reading the whole file.
- The layout **does not** merge adjacent segments of the same style: doing so would require
  concatenating strings and would lose the `Cow::Borrowed`. Avoiding redundant SGR sequences is
  the backend's job — it tracks the current style as state.
- `pulldown-cmark` emits the table header as bare cells, with no `TableRow`. The engine opens
  the row when it receives `TableHead`.
- `pulldown-cmark` does not number list items: the reader does, in `Marker::Ordered`.
- Raw HTML inside Markdown is dropped silently. Dumping `<br>` as text would be worse; the M3
  HTML reader is the one that knows how to interpret it.
- A reader must never decide how bytes become text: the encoding is resolved by the detection
  layer and applied to the `Source`, and readers call `decode_line`. Duplicating that judgement is
  how a latin-1 file ends up full of replacement characters when the right encoding was already
  known.
- `encoding_rs::decode` does BOM sniffing and **can override the encoding you asked for**. Use
  `decode_without_bom_handling` and leave BOM handling to `termdoc-detect`.
- Detection can name formats this build has no reader for. `run.rs::pick_reader` degrades to plain
  text with a warning instead of failing.
- The memory budget is measured in **the process's anonymous memory**, not RSS. With `mmap`, RSS
  tracks the file size through clean page-cache pages, and that is not memory the process owns
  (`docs/DESIGN.md` §8).
