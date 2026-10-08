# Handoff

> For whoever picks this up next, human or agent.
> Written 2026-08-10, refreshed 2026-10-08. The last code change was `dd2a640` (the JSON reader).

Read this first, then [`CLAUDE.md`](../CLAUDE.md) for the working rules, then
[`DESIGN.md`](DESIGN.md) when you need the *why* behind a structure.

---

## 1. Where things stand, in one screen

```
M0  ████████████████████  complete   Markdown, plain text, logs
M1  █████░░░░░░░░░░░░░░░  ~25%       detection + encoding landed; readers pending
M2  ░░░░░░░░░░░░░░░░░░░░             the TUI pager
M3  ░░░░░░░░░░░░░░░░░░░░             HTML, DOCX, ODT, RTF, EPUB, PDF, images
M4  ░░░░░░░░░░░░░░░░░░░░             plugin host and SDK
M5  ░░░░░░░░░░░░░░░░░░░░             PPTX, XLSX, Jupyter, SVG, math
```

| | |
|---|---|
| Repo | `git@github.com:marturojt/termdoc.git`, branch `main` |
| Commits | 15, history is clean and in English |
| crates.io | all 7 crates live at `0.1.0`; `cargo install termdoc` — see §10 |
| Site | [termdoc.app](https://termdoc.app), source in `marturojt/termdoc-site` (Next.js on Vercel) |
| Code | ~10,500 lines across 8 crates |
| Tests | **266**, all green |
| Lint | `clippy -D warnings` clean, `fmt` clean |
| CI | 6 jobs green on Linux/macOS/**Windows** |
| Startup | 3.9 ms (budget 10) |
| Own memory | 1.4 MB with 488 MB of input (budget 50 MB) |

**Everything is committed and pushed.** The working tree is clean; there is no
half-finished edit to reconstruct.

### What works from the command line today

```bash
termdoc README.md              # colorized, wrapped, tables aligned
termdoc README.md | head -40   # clean stream, dies with signal 13 like cat
cat README.md | termdoc        # detected by content, no filename needed
termdoc --explain odd.dat      # why that format, and which layers lost
termdoc --encoding latin1 x.txt
termdoc --ascii --width 40 t.md
```

Readers exist for **Markdown, plain text and logs**. Detection recognizes far more (JSON, YAML,
TOML, XML, HTML, CSV, source code, PDF, DOCX, ODT, EPUB, XLSX, PPTX, binaries) and anything textual
without its own reader falls back to plain text with a warning on stderr. That fallback is
deliberate, not an oversight — see §4.

---

## 2. Do this first

Five minutes to confirm nothing rotted, and it doubles as a tour:

```bash
cargo test --workspace                                   # expect 266 passing
cargo clippy --workspace --all-targets -- -D warnings    # expect silence
cargo build --release && python3 scripts/perf-gate.py    # expect 3 OK
target/release/termdoc corpus/basic.md                   # expect colors and a table
target/release/termdoc --explain corpus/tables.md        # expect a candidate list
```

If `cargo test` fails on a snapshot, `git diff crates/termdoc-cli/tests/snapshots/` says what
changed. If it fails anywhere else, that is a real regression and not an environment problem — this
suite has no known flakiness.

---

## 3. What to do next

The remaining M1 work, in the order I would keep.

### M1-1. Structured data readers — ~~JSON~~, YAML, TOML, XML  ← JSON landed; YAML next

`crates/termdoc-read-data/` **exists** and carries the JSON reader. Remaining dependencies, already
vetted: `yaml-rust2` 0.11, `toml` 1.1, `quick-xml` 0.41.

**Read `json.rs`'s module header before adding the next one.** It records the two things the
measurement caught: that a ceiling derived from the parser alone is four times too generous because
the pipeline costs more than `serde_json` does, and that the layout engine emits one line per `Text`
event inside `Preformatted`, which is why the output carries no per-token styling.

Points worth deciding deliberately rather than by default:

- **What "rendering" JSON means.** Almost certainly pretty-printing with indentation, keys in one
  theme role and scalars in another. `Tag::Preformatted` plus styled `Text` is likely enough; you
  probably do **not** want `CodeBlock`, because that will later imply syntax highlighting on top of
  structure you already understand.
- **Whether to stream.** Do not treat this as one decision for all four formats — the profiles are
  opposite, and §6.1 records the per-format answer. Do not silently claim streaming in
  `ReaderCaps`: nothing consumes that field today, so an aspirational `true` rots into a lie with no
  test to catch it.
- **YAML with `yaml-rust2`** is a low-level event parser, which fits the event model well. Do not
  reach for a `serde` DOM out of habit — and read the warning in §9 of DESIGN.md before touching any
  YAML crate.

### M1-2. CSV as a table

Emit `Tag::Table` and reuse the layout's width allocation — the interesting work is already done and
already tested. `termdoc_detect::detect_delimiter` gives you the delimiter and column count.

**This one can stream** (row by row), and it is worth doing so: a million-row CSV is a realistic
input. The catch is that `TableBuilder` buffers the whole table to allocate widths, so a huge CSV
needs either a row cap with an honest warning or a two-pass approach. Decide and document it.

### M1-3. Syntax highlighting

`syntect` 5.3 with `two-face` 0.5 (which bundles `bat`'s assets). Two jobs:

- A reader for source files (`FormatId::SourceCode`).
- A `Transform` over `CodeBlock` so fenced Markdown blocks get highlighted too.

**The startup budget is the whole difficulty.** Load assets lazily from the binary dump, only when a
code block actually appears. If `perf-gate.py` shows startup crossing 10 ms, the laziness is wrong —
that gate exists precisely to catch this.

`termdoc_detect::language_for(&src)` already resolves the grammar name from the extension, the
filename or the shebang.

### M1-4. Log reader and incremental stdin

Timestamp and severity recognition with per-level highlighting. `termdoc-detect` already recognizes
ISO-8601, bare clocks and syslog shapes in `structural.rs::starts_with_timestamp` — reuse that
rather than writing a second parser.

This is also where **incremental stdin** finally matters, and it is the one known limitation to
retire: `Source::from_stdin` buffers everything today (documented in `source.rs`). Until it is
incremental, `kubectl logs -f | termdoc` cannot work. Expect this to be the hardest item, because it
means an input path that is not a single `&[u8]`, and `Events<'a>` borrows from exactly that.

---

## 4. Decisions already made — do not silently re-decide these

Each one has a reason and a test. Overturn them if you have a better argument, but do it explicitly
and update DESIGN.md.

| Decision | Why | Where |
|---|---|---|
| Event stream, not an AST | Memory and huge files; a tree can be built from a stream, not the reverse | §2.2 |
| `Line` lives in `core`, not `layout` | It is the layout→backend contract, so the backend need not depend on layout | §13 |
| Grapheme integrity outranks the width limit | A ZWJ emoji is 2 cells and indivisible; splitting it makes garbage and does not fix the overflow | `wrap.rs` header |
| YAML may not claim STRUCTURAL confidence | `key: value` is indistinguishable from a Markdown options list; it would render READMEs as YAML | `structural.rs` header |
| CSV by delimiter variance, with a Markdown-table veto | Prose has commas; a GFM table has perfectly consistent pipes | `delimited.rs` |
| Flags beat the environment (`--color always` > `NO_COLOR`) | Ecosystem convention: ripgrep, bat, delta | `integration.rs` |
| Detected-but-unreadable degrades to plain text | Mid-roadmap, showing a `.json` as text beats refusing it | `run.rs::pick_reader` |
| Budget anonymous memory, not RSS | With `mmap`, RSS tracks file size through clean page-cache pages the process does not own | §8 |
| The TSV table rung gives up on width | Truncating loses data; that output is for `cut`/`awk`, not for reading | `table.rs` |
| Readers register no detectors | Otherwise detection order depends on which readers are compiled in | `read-text/src/lib.rs` |

---

## 5. Traps already paid for

These cost real debugging time. They are all fixed; this list exists so they are not rediscovered.

1. **`encoding_rs::decode` does BOM sniffing and overrides the encoding you asked for.** `FF FE` is
   a UTF-16LE BOM, so a UTF-8 source starting with those bytes got reinterpreted wholesale. Use
   `decode_without_bom_handling`.
2. **`infer` recognizes textual formats.** It answers `text/xml` for an XML declaration, and claiming
   that as binary at confidence 90 outranked the layer that can actually read it.
3. **Tests sharing a temp fixture path race.** Cargo parallelizes; one test opened a file in the
   instant another had truncated it to zero. The symptom looked like a detection bug. Use a
   per-call unique directory.
4. **`pulldown-cmark` emits the table header as bare cells**, with no wrapping `TableRow`. Without
   opening one on `TableHead`, the header gets no bold and no separator row.
5. **`.gitattributes` precedence is last-rule-wins.** A general rule placed after the exceptions
   silently normalized the corpus, and `corpus/plain.txt` deliberately contains a CRLF line.
6. **CI's clippy can be newer than the local toolchain.** With `-D warnings` that is a hard failure,
   so a green local clippy proves nothing. Read the CI log.
7. **Rust ignores `SIGPIPE`.** Without the `SIG_DFL` reset, `| head` panics. This is the single most
   important line in `main`.
8. **A reader that validates UTF-8 itself** throws away the encoding detection already did — latin-1
   rendered as replacement characters even though the right answer was known.

---

## 6. Open questions for the owner

1. **Streaming versus simplicity for the data readers.** Resolved in shape, not yet in code: it is
   **four decisions, not one**. XML streams because `quick-xml` is already a pull parser that
   borrows; TOML materializes without apology because config files are small; YAML uses
   `yaml-rust2`'s event parser; and **only JSON is a real dilemma**. For JSON the plan is
   `serde_json` plus a size guard — under the threshold it materializes and pretty-prints, over it
   the reader emits `Tag::Preformatted` and an `Event::Diagnostic` saying why. The guard lives in
   the reader, not in `run.rs`, because `pick_reader` decides by *format* and has no business
   knowing about byte counts. **Derive the threshold from a measurement** with
   `scripts/perf-gate.py`, not from a guess. Note also that YAML aliases (`*ref`) cannot be
   resolved by a pure stream: render the reference as written rather than expanding it.
2. **Publishing to crates.io.** Settled: all seven crates are live at `0.1.0`. See §10.

---

## 7. Environment notes for this machine

- **Rust 1.96.1 from Homebrew, and no `rustup`.** So no `rustup update`, and no cross-compiling to
  verify Windows locally — CI is the only Windows check. This is also why CI's clippy can be ahead.
- **`cargo-insta` is not installed.** Accept snapshots with `INSTA_UPDATE=always cargo test -p
  termdoc --test snapshots`, then **read `git diff` on the snapshot directory**. Accepting
  blindly defeats the point of having them.
- **`gh` 2.96 is available and authenticated**, which is how CI runs get watched.
- **`corpus/huge.log` is gitignored** (122 MB). `./scripts/gen-corpus.sh` regenerates it; the perf
  gate creates a smaller one on demand if it is missing.
- `hyperfine` is not installed and is not needed: `scripts/perf-gate.py` does its own timing.

---

## 8. How the docs relate

```
README.md         what termdoc is, for someone who just found it
CLAUDE.md         how to work in this repo: commands, invariants, gotchas
docs/DESIGN.md    the architecture and every non-obvious decision, with section numbers
docs/HANDOFF.md   this file: state, next steps, and what not to re-decide
```

`DESIGN.md` is the source of truth for structure. It has been **corrected twice** as implementation
found defects in it (§2.2 on the event model, §8 on the memory metric, §13 on the deviations), and
that is the intended relationship: when code and design disagree, one of them changes on purpose and
the reason gets written down.

---

## 9. What I would want to know if I were you

- The test suite is the actual specification. When unsure whether a behavior is intended, search the
  tests before reading the implementation — they carry the reasoning in their names and comments.
- The five test levels exist to tell failures apart. If a heading renders wrong, the event goldens
  say whether the reader misread it or the layout mislaid it, which saves guessing.
- `--explain` is the debugging tool for anything detection-related. It shows every layer's opinion,
  including the losers, and each one carries a human-readable reason.
- The design's section numbers are cited from code comments on purpose. If you change a decision,
  grep for the section number to find every place that leans on it.
- Do not trust a passing `clippy` locally as a green light for CI, and do not push a snapshot diff
  you have not read.

---

## 10. crates.io: published at 0.1.0

All seven crates are live as of 2026-08-10. Every candidate name was free when checked, **`termdoc`**
included, so `cargo install termdoc` works.

What was done:

- The CLI package was renamed `termdoc-cli` → **`termdoc`**, so `cargo install termdoc` works and
  the project's own name is not left for someone else to take. The **directory** keeps its `-cli`
  suffix — it names the layer, and `tests/layering.rs` indexes by directory, so the test was
  untouched. Reasoning in DESIGN.md §14.
- `keywords`, `categories`, `homepage` and `readme` added to every manifest.
- A README per crate. Each library's says plainly that **the API is unstable before 1.0**, which is
  the honest counterpart to publishing while §11 still gates 1.0 on freezing the document model and
  the plugin protocol. The libraries ship because a binary cannot be published with unpublished
  path dependencies — not because anyone should build on them yet.
- `cargo publish --workspace` derives the order itself: `core` and `term`, then `backend`, `detect`,
  `layout`, `read-text`, and `termdoc` last.

### The trap for the next release

**crates.io rate-limits *new* crates: a burst of five, then roughly one per ten minutes.** A single
`cargo publish --workspace` therefore cannot create seven crates in one go. It uploaded five, then
failed with `429` on the sixth, leaving the flagship name unclaimed — the two stragglers went up on
a retry loop over the following twenty minutes.

This only bites when *creating* crates. Publishing new **versions** of crates that already exist has
a far looser limit, so future releases are a single `cargo publish --workspace`. But when M1's
`termdoc-read-data` lands, or M2's `termdoc-tui`, that new crate hits the new-crate limit again:
publish it on its own first, then release the rest.

Also worth knowing: a published version can be yanked but never replaced or deleted, `cargo publish`
refuses a dirty working tree, and crates.io rejects the upload outright if the account's email is
not verified.

The crates.io, CI and docs.rs badges are in `README.md`. Note that `termdoc-read-data` is not on crates.io yet: it is a new crate, so it must be published on its own first.

---

## 11. Backlog — work that is not a milestone

The M1 items in §3 are the roadmap. These are not: they are release engineering, they do not block
any milestone, and they can be picked up whenever there is an appetite for something other than
readers. Kept here so they stop living in someone's head.

### B1. A Homebrew formula

**Why:** `cargo install termdoc` works, but it needs a Rust toolchain. Most people who want a
document viewer do not have one, and will not install one to get it.

**The whole plan, with its reasoning, is DESIGN.md §15.** The short version, in order:

1. ~~**Tag `v0.1.0` and cut a GitHub release.**~~ **Done (2026-08-11).** `.github/workflows/release.yml`
   builds macOS universal, Linux x86_64 and aarch64, and Windows on any `v*` tag, verifies each
   artifact by running it, and drafts a release carrying `SHA256SUMS.txt` — which is the file the
   formula reads. The URLs a formula needs now exist.
2. **Add `Formula/termdoc.rb` to the existing `marturojt/homebrew-tap`.** The tap already exists
   and already carries a working `dapctl.rb`, so there is no tap to create — users reach it as
   `brew install marturojt/tap/termdoc`.
3. **Copy the shape of `dapctl.rb`**, which ships prebuilt binaries per platform rather than
   building from source, and copy `dapctl`'s `.github/workflows/release.yml` that produces them.
   Both are proven and belong to the same author.
4. **Do not build from source in the formula.** The workspace sets `lto = "fat"` and
   `codegen-units = 1`, which is right for an artifact built once in CI and wrong for something
   every user compiles while waiting.
5. **Automate the formula bump** from the release workflow. A tap that lags its releases is worse
   than no tap.

**Not `homebrew-core` yet.** It applies a notability bar in stars, forks and watchers that a newly
published project does not clear. The existing tap now, core when there are users; the formula is
nearly the same file either way, so nothing is thrown away.

**Related, and cheaper:** static musl binaries attached to the release would serve Linux users who
have neither Rust nor Homebrew, and are a prerequisite for bottles anyway.
