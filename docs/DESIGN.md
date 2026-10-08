# termdoc design

> Architecture document. Status: **M0 complete, M1 in progress**.
> Last updated: 2026-10-08.

`termdoc` is a universal document viewer for the terminal. It reads any document and renders it
as well as the terminal allows, never opening an external application and degrading gracefully
based on the terminal's real capabilities.

> **This document describes the target, not today.** It is the architecture the project is being
> built towards, so it speaks of readers and a TUI that do not exist yet. For what actually works
> in the current build, the README's format table is the honest answer, and `termdoc --formats` is
> the authoritative one. §11 says which milestone brings what.

It is not an editor. It is not a converter. It is not an IDE.

## Principles

1. Do one thing and do it well: turn documents into readable terminal output.
2. Fast: near-instant startup.
3. Low memory, even with enormous files.
4. Highly extensible.
5. Correct with pipes and scripts.
6. Feels like a native system utility.
7. **Graceful degradation is a principle, not a detail.**

## Framing decisions

| Axis | Decision |
|---|---|
| Language | **Rust** (2024 edition) |
| Plugins | **Hybrid**: a compile-time trait registry plus external plugins over stdio |
| Heavy formats | **Native first**, with optional and never-required external helpers |
| Interaction | **Dual, auto-detected**: a TUI when stdout is a terminal, an ANSI stream when it is a pipe |

### Two tensions the design resolves explicitly

1. **"Do one thing well" vs. 15 formats + a TUI + plugins + 5 backends.** The *one thing* is
   *turning documents into readable terminal output*. Unix is honored by keeping the core minimal
   and pushing **everything** else into separate crates behind feature flags: a
   `--no-default-features` build with Markdown only pulls in not a single line of PDF code. The
   default binary is a convenient superset, not a monolith.
2. **"Streaming and enormous files" vs. PDF/DOCX/EPUB.** Those formats are *random access* by
   construction (PDF's xref, ZIP's central directory): they cannot be streamed. The design
   accepts that and parses them **lazily, per page or per section**, rather than pretending the
   whole pipeline streams. A 2 GB log will be free; a 2 GB PDF never will be, and that is a
   property of the format, not of the design.

---

## 1. Use cases

These define the API. Each one has to work the day its milestone closes.

```bash
# Direct reading — picks a reader and decides TUI vs. stream based on the destination
termdoc report.pdf                  # TTY  → interactive pager
termdoc README.md | head -40        # pipe → ANSI stream, no screen control
termdoc data.csv > out.txt          # pipe → plain text, no ANSI

# stdin with no filename: detection by content
cat README.md   | termdoc
curl -sL https://example.com/a.pdf | termdoc
git show HEAD:README.md | termdoc
kubectl logs pod | termdoc -f       # follow mode

# One-off queries, without opening the TUI
termdoc --toc book.epub             # the table of contents only
termdoc --meta report.docx          # the metadata only
termdoc --page 12-18 thesis.pdf     # a page range
termdoc --explain odd.dat           # why it chose that format, then exit

# Unix composition
termdoc *.md | grep -i TODO
termdoc -t markdown manual.docx > manual.md   # an alternative backend
```

**Scripting invariant:** with stdout redirected, stdout contains **only** the document. Warnings,
diagnostics and progress go to stderr. Always.

### CLI surface

```
termdoc [OPTIONS] [FILE]...
  -f, --from <FMT>         override detection
  -t, --to <BACKEND>       ansi | plain | markdown | html   (default: auto)
      --color <WHEN>       auto | always | never
      --width <N>
      --theme <NAME>
      --ascii              no Unicode
      --images <MODE>      auto | kitty | iterm2 | sixel | blocks | none
  -p, --paginate <WHEN>    auto | always | never
  -n, --line-numbers
      --toc                print only the table of contents
      --page <RANGE>       a page or section range
      --search <PATTERN>   open the TUI at the first match; filter in stream mode
      --meta               print only the metadata
      --explain            print the detection reasoning and exit
      --encoding <ENC>     override encoding detection
      --strict             turn warnings into errors
      --plugins            list the discovered plugins
      --no-plugins
```

Configuration lives in `~/.config/termdoc/config.toml` plus `TERMDOC_*` variables. Precedence:
flags > env > project config > user config > defaults. With several files, they are concatenated
with a separator header (`bat` style), not in tabs.

---

## 2. Architecture

### 2.1 The pipeline

Two refinements over the initial sketch: *Renderer* is renamed to *Reader* (it produces the
model, it does not paint), and **Layout** is added, which is where the hard work lives.

```
Source ──▶ Detect ──▶ DocumentReader ──▶ Event stream ──▶ Transforms ──▶ Layout ──▶ Backend ──▶ Sink
  │           │             │                                  │            │          │         │
mmap /     magic +      registry +                        highlight,     width,     ANSI /    TUI  or
stdin      sniff +      plugins                            TOC, filter,  wrap,      Plain /   stdout
buffer     charset                                         search        tables     Kitty…
```

- **Layout** is the only stage that knows the viewport width and the fidelity level. Sharing it
  means *every* format inherits good Unicode wrapping, aligned tables and list indentation for
  free. Without it, every reader would reimplement the same thing badly.
- **No reader knows any backend.** This is not a convention: it is a guarantee from Cargo's
  dependency graph (§3).

### 2.2 The internal model: an event stream, not a tree

**The project's central decision.** The document model is an **event stream** in the style of
`pulldown-cmark`, not a materialized AST:

```rust
pub enum Event<'a> {
    Start(Tag<'a>),
    End(TagKind),
    Text(Cow<'a, str>),
    Code(Cow<'a, str>),
    Image { source: ImageSource<'a>, alt: Cow<'a, str>, dims: Option<(u32, u32)> },
    Math { inline: bool, tex: Cow<'a, str> },
    FootnoteRef(Cow<'a, str>),
    Break(BreakKind),
    Rule,
    PageBreak,
    TaskMarker(bool),
    Diagnostic(Diagnostic),   // a recoverable error, inline in the stream
}

pub type Events<'a> = Box<dyn Iterator<Item = Result<Spanned<Event<'a>>>> + 'a>;
```

> **Correction over the first version of this design.** The approved version separated
> `Event::Inline(Inline)` from `Event::Start(Block)`, with `Emphasis`, `Strong` and `Link` inside
> `Inline`. **That cannot be expressed:** emphasis *wraps* content, so it needs an open and a
> close just like a paragraph, and with a single `Inline(Emphasis)` there is no way to know where
> it ends.
>
> Everything is therefore unified into `Start(Tag)` / `End(TagKind)` for **every** container —
> block-level or inline — with leaf events reserved for whatever wraps nothing. This is
> `pulldown-cmark`'s proven model, and it has the side benefit of making the mapping from its
> output nearly mechanical.
>
> `Tag::Preformatted` was also added, which was missing: plain text and logs need a block whose
> source line breaks are meaningful and must **not** be reflowed, distinct from `CodeBlock`, which
> additionally highlights syntax.

> **Second correction: tokens, and lines that are assembled.** Inside `Preformatted` and
> `CodeBlock` the first implementation took every `Text` event as one complete line, which made
> it impossible for a JSON key and its value to be different colors: marking them with an inline
> tag put each on a line of its own. Two changes together fix it.
>
> - `Tag::Token { role: TokenRole }` is an inline container naming what a span *is* — `Key`,
>   `String`, `Number`, `Bool`, `Null`, `Punctuation`, `Comment`, `Name`, `Attribute`. Roles are
>   syntax, never colors: the reader describes, the theme (`Theme::token_style`) decides.
> - **In preformatted content, text accumulates until a newline closes the line.** A `Text`
>   without a trailing newline stays open and the next one continues it; the block's `End`
>   closes whatever is left, without adding a blank line. A reader therefore **keeps the
>   terminator on each line** it emits (`"one\n"`), which costs nothing — the slice still
>   borrows from the `mmap` — and the layout trims `\n` and `\r\n` itself.
>
> A syntax highlighter (M1-3) is the same shape: one line, several styled runs.

Reasons, in order of weight:

1. **Memory and enormous files.** A 10M-line log or a 1M-row CSV is never materialized.
2. **Real zero-copy.** `Cow<'a, str>` borrows straight from the input `mmap`; `'a` is the
   `Source`'s lifetime.
3. **Backends become trivial to test.** A backend is a state machine over events.
4. **Composable transforms.** Highlighting, filtering and search marking are iterator adapters,
   chainable at no cost.

Whatever needs random access (the TOC, a table's column widths, the TUI's scrollback) is obtained
through a `TreeBuilder` adapter that materializes **only the necessary subtree**. A tree can be
built from events; a tree cannot be streamed. The direction of the conversion is what matters.

**Lazy images.** `ImageSource` is a *handle*, never pixels:
`Path | Bytes(Arc<[u8]>) | Entry { container, name }`. Nothing is decoded if the backend cannot
display graphics — only then is the handle resolved. Decoding a 4000×3000 PNG only to print
`[image: diagram]` is exactly the kind of waste this project cannot afford.

**Location for navigation and search.** Every event carries
`Span { start, end, line: Option<u32>, page: Option<u32> }`. Without it there is no `--page`, no
jumping from a search hit, and no navigable TOC — and adding it later would mean touching every
reader.

**Metadata:** `title`, `authors`, `date`, `language`, `page_count`, `word_count`,
`source_format`, `encoding`, and a `custom` map for format-specific extras.

### 2.3 Public traits

```rust
// Produces the model. Lives in termdoc-core; cannot see backends.
pub trait DocumentReader: Send + Sync {
    fn id(&self) -> FormatId;
    fn read<'a>(&self, src: &'a Source, ctx: &ReadContext) -> Result<Events<'a>>;
    fn capabilities(&self) -> ReaderCaps;   // streaming? random access? pages?
}

pub trait Detector: Send + Sync {
    fn sniff(&self, src: &Source) -> Option<Detection>;   // with confidence 0..=100
}

pub trait Transform: Send + Sync {
    fn apply<'a>(&self, events: Events<'a>) -> Events<'a>;
}

// Consumes laid-out lines. Cannot see readers.
pub trait Backend {
    fn caps(&self) -> BackendCaps;   // color depth, graphics, OSC-8, unicode
    fn begin(&mut self, out: &mut dyn Write) -> Result<()>;
    fn write_line(&mut self, line: &Line<'_>, out: &mut dyn Write) -> Result<()>;
    fn finish(&mut self, out: &mut dyn Write) -> Result<()>;
}
```

`Source` exposes `peek(n) -> &[u8]` so detection can happen without consuming. For stdin this
requires **buffering and replaying the prefix** (~8 KB); it is the detail that makes
`curl | termdoc` work and it is usually forgotten.

### 2.4 The registry — and why a plugin is indistinguishable from a built-in

```rust
pub struct Registry { readers: Vec<Arc<dyn DocumentReader>>, detectors: Vec<Arc<dyn Detector>> }
impl Registry {
    pub fn register_reader(&mut self, r: Arc<dyn DocumentReader>);
    pub fn reader_for(&self, format: FormatId) -> Option<&dyn DocumentReader>;
}
```

The plugin host wraps each external plugin in a `ProxyReader` that **implements
`DocumentReader`**. The core therefore has exactly one code path: there is no "is this a plugin?"
branch. That is the property that makes the plugin system cheap to maintain.

---

## 3. Repository structure

A Cargo workspace. Splitting into crates is not cosmetic: **the dependency graph makes violating
the layering impossible.**

```
termdoc/
├── Cargo.toml                    # workspace + release/bench profiles
├── CLAUDE.md  README.md  LICENSE-MIT  LICENSE-APACHE
├── crates/
│   ├── termdoc-core/             # Event/Tag/Line, traits, Registry, Source, Error
│   ├── termdoc-detect/           # detectors: magic, zip-inner, text sniffing, charset
│   ├── termdoc-layout/           # layout engine: wrapping, tables, lists, fidelity
│   ├── termdoc-term/             # terminal capabilities, probing, caching
│   ├── termdoc-backend/          # Ansi, Plain, Markdown, Html + graphics
│   ├── termdoc-read-text/        # txt, markdown, log, code (syntect)
│   ├── termdoc-read-data/        # json, yaml, toml, xml, csv
│   ├── termdoc-read-markup/      # html
│   ├── termdoc-read-office/      # docx, odt, rtf, xlsx, pptx (the ZIP+XML family)
│   ├── termdoc-read-epub/        # epub (reuses zip + markup's XHTML parser)
│   ├── termdoc-read-pdf/         # pdf, with a swappable engine
│   ├── termdoc-tui/              # the interactive pager (ratatui)
│   ├── termdoc-plugin/           # host: discovery, manifest cache, stdio protocol
│   ├── termdoc-plugin-sdk/       # a publishable crate for plugin authors
│   └── termdoc-cli/              # the binary: clap, config, wiring, exit codes
│                                 #   (published as the `termdoc` package — see §14)
├── scripts/                      # corpus generation, performance gates
├── corpus/                       # test documents (small, clearly licensed)
├── tests/  benches/  fuzz/
└── docs/adr/                     # numbered architecture decisions
```

The permitted dependency directions — **anything else is a compile error**:

```
termdoc-cli        → everything
termdoc-read-*     → core                    (does NOT see backend, layout or term)
                                             read-text: md/txt/log · read-data: json
termdoc-backend    → core, term
termdoc-layout     → core, term
termdoc-tui        → core, layout, backend, term
termdoc-plugin     → core
termdoc-core       → (nothing internal)
```

A CI test (`crates/termdoc-cli/tests/layering.rs`) pins these edges so the layering does not erode
over time. It reads the manifests rather than invoking `cargo tree`: it is instant, needs no
network, and the failure points at the exact file to fix.

---

## 4. Format detection

Layered; the first sufficiently confident layer wins. Every step records its reasoning for
`--explain`.

| # | Layer | Notes |
|---|---|---|
| 1 | `--from <fmt>` | Overrides everything |
| 2 | **Magic bytes** | `%PDF-`, `{\rtf`, `PK\x03\x04`, `\x89PNG`… via `infer` plus our own table |
| 3 | **Intra-ZIP disambiguation** | Essential: a ZIP may be DOCX/ODT/EPUB/XLSX/PPTX/plain zip |
| 4 | Extension | Only when 2–3 are inconclusive (text formats have no magic) |
| 5 | **Structural sniffing** | Over the first 8 KB |
| 6 | Shebang / modeline | Then the extension→language map from `two-face`/syntect |
| 7 | Fallback | Valid UTF-8 → text; binary → hex dump |

**Step 3, concretely:** read the ZIP's central directory and decide by `mimetype` (EPUB =
`application/epub+zip`, ODT = `application/vnd.oasis.opendocument.text`) or by a characteristic
entry (`word/document.xml` → DOCX, `xl/workbook.xml` → XLSX, `ppt/presentation.xml` → PPTX).
Without this, all five ZIP formats are indistinguishable.

**Step 5, the case nearly every tool gets wrong — CSV.** "Are there commas?" is not enough. For
each candidate delimiter (`,` `;` `\t` `|`), count the fields per line over the first N lines and
measure the **variance**; the correct delimiter yields zero variance (respecting quotes). High
variance across all of them → it is not CSV. The rest: JSON (`{`/`[` plus actually parsing the
prefix), YAML (`---`, `key:` in column 0), TOML (`[section]`, `k = v`), XML/HTML (`<?xml`,
`<!DOCTYPE html`), Markdown (`#`, fences, lists), logs (a timestamp regex at line start).

**Encoding:** BOM → "does it parse as UTF-8?" → `chardetng` over 64 KB → transcoding with
`encoding_rs`, cached in the `Source`. Overridable with `--encoding`. Latin-1 documents are
common, and exiting with mojibake is not acceptable.

The order is not arbitrary. A BOM is a *declaration* by whoever wrote the file, so it wins
outright. Valid UTF-8 is then itself strong evidence — long invalid-UTF-8 runs are statistically
unlikely — and it must be checked *before* the statistical detector, or accented UTF-8 gets
"corrected" into windows-1252 and `é` becomes `Ã©`.

Two implementation notes worth recording:

- `Source::set_encoding` takes `&mut self`, so applying an encoding must happen before any reader
  borrows the source. The borrow checker enforces the ordering that a comment would only ask for.
- `encoding_rs::decode` performs **BOM sniffing and can override the requested encoding**: the
  bytes `FF FE` are a UTF-16LE BOM, so a UTF-8 source starting with them would be reinterpreted
  wholesale. `decode_without_bom_handling` is used instead, and BOM handling stays in the
  detection layer where it belongs.

**Which formats may claim STRUCTURAL confidence** — only those whose prefix can be *checked*
rather than guessed at: JSON, XML/HTML, TOML (a `[section]` header) and CSV. **YAML deliberately
may not.** `key: value` lines are indistinguishable from a Markdown document listing options, and
letting YAML claim 70 would beat a `.md` extension at 50 and render a README as YAML. YAML claims
HEURISTIC only, and only with an explicit `---` or `%YAML` marker; otherwise it relies on its
extension, which is what people actually have.

---

## 5. Terminal capabilities and graceful degradation

Degradation is a project principle, so it is modeled as a first-class type rather than as
scattered `if`s:

```rust
pub struct Fidelity { color: ColorDepth, graphics: GraphicsProto, unicode: UnicodeLevel, hyperlinks: bool }
```

`termdoc-layout` **receives** `Fidelity` as an input. That is what makes every rung verifiable
with a snapshot test.

**The degradation ladders:**

```
Graphics:  Kitty / iTerm2 / Sixel → Unicode half-blocks → Braille/ASCII art → [image: alt, 800x600]
Tables:    Unicode box-drawing    → ASCII +-|           → aligned TSV
Color:     truecolor → 256 → 16 → bold/underline only → nothing
Links:     OSC 8 → text plus a numbered reference list [1]
Headings:  styled → the source's own ATX notation (`##`)
```

Token roles (JSON keys, numbers, comments) are not a ladder of their own: they are ordinary styles,
so they descend the **Color** ladder with everything else, and with `ColorDepth::None` they emit no
escape byte. Without color they are indistinguishable from the text around them, which is honest —
the structure is carried by the indentation, not by the paint.

**Detection:** `IsTerminal` for the TTY check; `COLORTERM`, `TERM` and terminfo for color;
`NO_COLOR`/`CLICOLOR`/`CLICOLOR_FORCE` honored. For graphics, an environment sniff
(`TERM=xterm-kitty`, `KITTY_WINDOW_ID`, `TERM_PROGRAM`, `VTE_VERSION`) and, **only on an
interactive TTY**, an active probe: the protocol query fenced with `\e[c` (DA1) and a short
timeout. The result is cached in `~/.cache/termdoc/caps` keyed by `TERM` plus version, so the
probe is paid once rather than on every startup. Width: `terminal_size` → `COLUMNS` → 80.

---

## 6. The plugin system

### Discovery and startup

An obvious risk: if the core launches every plugin at startup to ask what it can do, the
"near-instant startup" requirement dies. The solution:

1. Every plugin ships a `plugin.toml` declaring its formats, extensions and magic bytes.
2. The core reads only those manifests (cheap TOML) and caches a merged index in
   `~/.cache/termdoc/plugins.idx`, invalidated by the directory's mtime.
3. **A plugin process is spawned only when its format is actually selected.** Cost in the common
   case: zero.

Search paths: `$TERMDOC_PLUGIN_PATH`, `~/.config/termdoc/plugins/`, `/usr/lib/termdoc/plugins/`,
plus `termdoc-<name>` executables on `PATH` (git style).

### The protocol

Length-prefixed MessagePack frames over stdin/stdout (`rmp-serde`). A handshake carries the
protocol version plus capabilities. The host passes the file path (so the plugin can do its own
`mmap`) or the bytes over a pipe when the source is stdin. The plugin returns `Event` frames —
**the very same enum from `termdoc-core`**, with `serde` behind a feature flag, so there are never
two definitions that can drift apart.

Isolation: timeouts, memory limits and frame-size limits are enforced by the host. A plugin that
dies does not take `termdoc` down: it degrades to the fallback reader and emits a `Diagnostic`.
That is the concrete advantage of having chosen subprocesses over `dlopen`.

`termdoc-plugin-sdk` exposes `run_plugin(impl DocumentReader)` — a Rust plugin is roughly 20
lines. The protocol is documented in `docs/plugin-protocol.md` for plugins in any language.

---

## 7. Error handling

- `thiserror` per crate; `miette` in the CLI for span-carrying diagnostics (`malformed JSON at
  line 42, column 7`, with the fragment highlighted).
- **Never panic on malformed input.**
  `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` in every `termdoc-read-*`
  crate. One `cargo-fuzz` target per reader.
- **Partial rendering beats total failure.** A corrupt PDF page or an invalid table row emits an
  `Event::Diagnostic` and the stream continues. On a TTY the diagnostics appear inline and dimmed
  with `⚠`; in a pipe they go to stderr and stdout stays clean.
- **Reset `SIGPIPE` to `SIG_DFL` at startup.** Rust ignores it by default, so
  `termdoc huge.pdf | head` would blow up with a broken-pipe panic. It is the classic bug in Unix
  utilities written in Rust, and it has to be handled on the first line of `main`.

**Exit codes:** `0` success (warnings included) · `1` generic error · `2` usage error ·
`3` unsupported format · `4` unreadable/corrupt source · `5` plugin failure · `130` SIGINT.
`--strict` promotes warnings to errors.

---

## 8. Performance

The budgets, treated as tests rather than aspirations:

| Metric | Target | Measured in M0 | Verification |
|---|---|---|---|
| Startup, small Markdown | < 10 ms | **3.4 ms** (median of 60) | `scripts/perf-gate.py`, fails on regression |
| Own memory, 488 MB log | < 50 MB | **1.42 MB** | pathological corpus |
| Lazy output (`\| head -5` over 122 MB) | must not read the whole file | **5.7 MB RSS, 9 ms** | integration test |
| First screen, 2000-page PDF | < 150 ms | — (M3) | lazy per-page parsing |

### Correcting the memory metric

The original budget said "peak RSS < 50 MB". **That metric is wrong for an `mmap`-based design**,
and it is worth recording why, because M0's very first measurement violated it by a factor of 2.5
with nothing actually wrong.

With the document memory-mapped, walking it end to end leaves its pages resident, so RSS ends up
tracking the file size. But those are **clean, file-backed pages**: the kernel reclaims them
instantly and they are shared with the page cache. They are not memory the process owns.

What does measure the thing that matters is anonymous memory — "peak memory footprint" on macOS,
`VmRSS` minus the mapped pages on Linux — and there the behavior is the one we were after:

| Input | Size | Own memory | RSS |
|---|--:|--:|--:|
| `basic.md` | 662 B | 1.20 MB | 2.2 MB |
| `huge.log` | 122 MB | 1.23 MB | 124 MB |
| `mega.log` | 488 MB | **1.42 MB** | 490 MB |

The input grows **750,000×** and the process's own memory **1.18×**. It is flat, which is the
property we wanted.

The corrected metric, therefore: **the process's anonymous memory is what gets budgeted**; RSS is
recorded as informational and grows with the bytes touched. Two practical consequences:

- `madvise(MADV_SEQUENTIAL)` is applied when mapping. On Linux it helps the kernel drop pages
  behind us; on macOS it was measured to make no difference to RSS. It is kept because the hint is
  free and correct.
- If RSS ever needs to be flat too — a cgroups environment that counts mapped pages, say — the
  route is reading in blocks instead of mapping, **and only in the streaming readers**. It costs
  the `Cow::Borrowed` zero-copy, so it will not be done without a measured reason. To be
  reassessed in M1 with the log reader.

How the rest is achieved:

- **Everything lazy.** syntect's assets are loaded only when a code block appears, from a binary
  dump (`two-face`, the same approach that gives `bat` its speed). No theme, grammar or font is
  touched if the document does not need it.
- **`mmap` + `Cow`.** `memmap2` lets the OS page in only what gets rendered; `Cow<'a, str>`
  borrows from the mapping without copying.
- **Streaming first.** In pipe mode nothing is buffered beyond one laid-out line and the detection
  prefix. In the TUI, a sliding window plus a line-offset index, so jumping to the end of a huge
  file does not require holding all of it in memory.
- **`rayon` with judgment.** Only for embarrassingly parallel work (highlighting independent code
  blocks, decoding several images). Never on the hot path of a small file, where spinning up the
  thread pool would eat the startup budget.
- Release profile: `lto = "fat"`, `codegen-units = 1`, `panic = "abort"` for the binary.

Benchmarks with `divan`. The mandatory pathological corpus: a 10M-line log, a 1M-row CSV, a
2000-page PDF, deeply nested JSON, Markdown with 5000 links, and a file with 1 MB lines and no
breaks.

---

## 9. Recommended dependencies

Verified on crates.io on 2026-08-10 (stable version and 90-day downloads).

| Area | Crate | Ver. | Note |
|---|---|---|---|
| CLI | `clap` (derive) | 4.6 | |
| Errors | `thiserror` / `miette` | 2.0 / 7.6 | lib / CLI |
| Markdown | `pulldown-cmark` | 0.13 | It *is* an event stream: a perfect fit for the internal model |
| Syntax | `syntect` + `two-face` | 5.3 / 0.5 | `two-face` bundles `bat`'s assets |
| TUI | `ratatui` + `crossterm` | 0.30 / 0.29 | `crossterm` covers Windows |
| Images | `ratatui-image` | 11.0 | Kitty + iTerm2 + Sixel + half-blocks. **Preferred over `viuer`**, which "dumps" the image and does not cohabit with a TUI |
| XML | `quick-xml` | 0.41 | Streaming; the basis of DOCX/ODT/EPUB/XLSX |
| ZIP | `zip` | 8.6 | |
| HTML | `lol_html` or `html5ever` | 3.0 / 0.39 | `lol_html` streams; `html5ever` is more faithful |
| Data | `serde_json` (`toml` was vetted, unused: see ADR 7) | 1.0 | |
| YAML | *(none — see the note below)* | — | A line-by-line highlighter; `yaml-rust2` was the plan |
| CSV | `csv` | — | |
| Spreadsheets | `calamine` | 0.36 | XLSX/ODS, for M5 |
| PDF | `pdf_oxide` / `pdf-extract` | 0.3 / 0.12 | Swappable engine, see below |
| Detection | `infer` + our own | 0.22 | |
| Encoding | `encoding_rs` + `chardetng` | 0.8 / 1.0 | |
| Unicode | `unicode-width`, `unicode-segmentation`, `textwrap` | 0.2, 1.13, 0.16 | |
| mmap | `memmap2` | 0.9 | |
| Plugins | `rmp-serde` | 1.3 | |
| Parallelism | `rayon` | 1.12 | Restricted use (§8) |
| Tests | `insta`, `proptest`, `divan`, `cargo-fuzz` | | |

**YAML warning — this corrects a very common default choice:** `serde_yaml` is deprecated *and*
its fork `serde_yml` was archived over unsoundness issues. Use neither. `yaml-rust2` (0.11, ~13M
downloads/90d) is the established option and, being a low-level event parser with no `serde` DOM,
fits our stream model better than typed deserialization would. Live alternatives if `serde` is
wanted: `serde-saphyr` or `noyalib`.

**PDF warning:** `pdf_oxide` (0.3.77) is very fast and gaining traction, but it is young;
`pdf-extract` (0.12) is more established. **Do not marry either:** define a `trait PdfEngine` with
implementations behind features, and treat `pdftotext`/`mutool` as an *optional* improvement when
present on `PATH` (never required, and never opening a viewer). PDF text extraction is the weakest
point in the whole Rust ecosystem, and being able to swap engines without touching anything else
matters.

---

## 10. Testing strategy

1. **Event-stream golden tests, per reader.** The `Event` sequence is asserted with no ANSI
   involved. This isolates parsing bugs from painting bugs.
2. **Snapshot tests (`insta`) of the output.** A `corpus file × width × fidelity level` matrix.
   This is what turns the degradation ladder (§5) into something *verifiable* rather than a
   promise.
3. **Property tests (`proptest`) of the layout invariants:** no line exceeds the width in display
   cells; wrapping never splits a grapheme cluster; every ANSI sequence is balanced and reset.
4. **Fuzzing** per reader (`cargo-fuzz`), with no panics and no OOM.
5. **Cross-platform CI** on Linux/macOS/Windows, plus a matrix of terminal capabilities simulated
   through environment variables.
6. **Performance gates** via `scripts/perf-gate.py` against the budgets in §8.

---

## 11. Roadmap

Every milestone leaves a **usable** tool, not scaffolding.

| M | Scope | Result |
|---|---|---|
| **M0 Foundations** | Workspace; `Event`/`Tag`/`Line`; `Source` (mmap + stdin); layout engine (wrapping, lists, tables, code); ANSI + Plain backends; `termdoc-term`; the CLI; the snapshot harness. Formats: TXT, Markdown | `termdoc README.md` already beats `cat`; pipes behave correctly |
| **M1 Data and code** | JSON, YAML, TOML, XML, CSV, syntax-highlighted code, logs — all streaming. The complete detection engine with `--explain` | Covers the majority of daily use. **In progress:** `termdoc-detect` landed |
| **M2 Pager** | A `ratatui` TUI: scrolling, search, TOC navigation, follow mode (`-f`), keybindings. TTY/pipe auto-detection | Replaces `less` for documents |
| **M3 Rich documents** | HTML, DOCX, ODT, RTF, EPUB (sharing the ZIP+XML infrastructure), PDF. Images and graphics backends | Closes out the main format list |
| **M4 Extensibility** | The plugin host, protocol v1, the SDK, the manifest cache, a reference plugin. Markdown and HTML backends | Third parties add formats without touching the core |
| **M5 Beyond** | PPTX, XLSX, Jupyter, SVG, diagrams, math, scientific formats | |

Versioning stays at `0.x` through M3; `1.0` once the plugin protocol and the document model
freeze. Distribution — `cargo install` (live), Homebrew, AUR, Scoop and static musl binaries — is
planned in §15; it runs alongside the milestones rather than inside one.

---

## 12. Decisions to record as ADRs

The ones with real tension, worth writing down in `docs/adr/`:

1. **An event stream instead of an AST.** Memory and streaming outweigh random access; the tree is
   obtained through an adapter when needed.
2. **Layout as a shared stage.** It is where typographic quality lives.
3. **Layering enforced by the crate graph**, not by convention.
4. **Plugins launched lazily via manifests**, so instant startup is not sacrificed.
5. **Subprocesses over `dlopen`:** serialization is paid for in exchange for fault isolation and a
   stable ABI.
6. **A swappable PDF engine behind a trait**; the ecosystem has no clear winner yet.
7. **YAML: neither `serde_yaml` nor `serde_yml`** (both dead), and in the end not `yaml-rust2`
   either. Any parser's events drop the comments and normalise the rest, and a config file's
   comments are most of what a viewer is for. The reader highlights the text instead of parsing
   it (`termdoc-read-data/src/yaml.rs`): byte-for-byte output, truly streaming, no dependency.
   It is a lexer, not a validator; `yaml-rust2` remains the answer if validation is ever wanted.
   **TOML takes the same road** for the same reason (`termdoc-read-data/src/toml.rs`); the two
   share their line-by-line plumbing in `highlight.rs`.
8. **`SIGPIPE` reset to `SIG_DFL`.**
9. **`Renderer` renamed to `DocumentReader`;** "renderer" is reserved for layout+backend.
10. **Grapheme integrity outranks the width limit.** The two collide for a ZWJ emoji in a
    one-column terminal; splitting a cluster produces visible garbage and does not even fix the
    overflow.

---

## 13. M0 status and recorded deviations

**M0 is closed.** 164 tests green, `clippy -D warnings` clean, and the acceptance criteria below
verified.

Three deviations from what was approved, with their reasons:

1. ~~**`termdoc-detect` does not exist yet.**~~ **Resolved in M1.** The crate now exists with all
   four layers, intra-ZIP disambiguation, CSV delimiter-variance sniffing and encoding detection.
   Because detection can now name formats this build has no reader for, `pick_reader` degrades to
   plain text with a warning rather than refusing: mid-roadmap, showing a `.json` file as text
   beats refusing to show it at all, and a universal viewer should always show *something*.

2. **stdin is fully buffered.** M0's formats gain nothing from incremental streaming: Markdown
   needs the complete document anyway, and huge files have the file path, which *is* lazy. The
   incremental stdin reader arrives with the log reader in M1, where `kubectl logs -f | termdoc`
   makes it essential.

3. **`Line` lives in `termdoc-core`, not `termdoc-layout`.** §3's table gave
   `termdoc-backend → core, term`, but the backend consumes `Line`. Putting `Line` in `core` —next
   to `Event`— resolves the contradiction and is more coherent besides: `Event` is the
   reader→layout contract and `Line` the layout→backend contract; both are shared vocabulary. This
   is what keeps the backend from depending on the layout engine.

Plus one decision the design did not settle and the implementation had to:

- **`NO_COLOR` vs. `--color` precedence:** flags beat the environment, as in ripgrep, bat and
  delta. With `--color auto` (the default) `NO_COLOR` is always honored; `--color always` can
  override it.

### Acceptance criteria

At M0's close, all of this must pass:

```bash
cargo test --workspace                      # unit + golden + snapshots
cargo test -p termdoc --test layering       # the dependency-graph edges
cargo clippy --workspace --all-targets -- -D warnings

termdoc corpus/basic.md                     # colors, Unicode tables
termdoc corpus/basic.md | cat               # a plain stream, no ANSI
termdoc corpus/huge.log | head -5           # no SIGPIPE panic  ← critical gate
cat corpus/basic.md | termdoc               # content-based detection on stdin
NO_COLOR=1 termdoc corpus/basic.md | cat    # no escape sequences
termdoc --width 40 --ascii corpus/tables.md # a degradation rung
python3 scripts/perf-gate.py                # startup, memory, lazy output
```

---

## 14. Publishing

**Done: all seven crates are live on crates.io at `0.1.0` since 2026-08-10**, and the name that
matters is claimed by the binary. What follows is why it is shaped this way.

### The package is `termdoc`, the directory is `crates/termdoc-cli/`

`cargo install termdoc` has to work, and the project's own name should not sit unclaimed while
someone else takes it. So the CLI package is named `termdoc`, following the ecosystem's convention
(`ripgrep` publishes as `ripgrep` and installs `rg`; `bat` publishes as `bat`).

The **directory** keeps the `-cli` suffix, because it names the *layer*, and the layering table in
§3 and `tests/layering.rs` are both organized by layer. That test indexes by directory name, so the
package rename does not touch it. The mismatch is deliberate and costs one line of explanation here.

### 0.x is the API contract

Every crate is published at `0.1.0`, and each library's README says the API is unstable before 1.0.
That is not boilerplate: §11 gates 1.0 on freezing the **document model** and the **plugin
protocol**, and neither is frozen. `Event`, `Tag` and the reader traits will still change as M1's
readers land and M4's plugin host is built — publishing now claims the names and lets people install
the binary, without promising a stability that does not exist yet.

The libraries are published because a workspace cannot publish a binary that depends on unpublished
path dependencies, not because anyone should be building on them yet.

### Order

`cargo publish --workspace` computes the dependency order and waits for the index between crates.
The order it derives is `core` and `term` first, then `detect`, `layout`, `backend` and
`read-text`, then `termdoc` last.

Publishing is irreversible: a version can be yanked, but never replaced or deleted, and the name is
taken forever. Run `cargo publish --workspace --dry-run` and read the packaged file list first.

---

## 15. Distribution

`cargo install termdoc` works today and is the baseline. Everything below exists because it
requires a Rust toolchain, which most people who would use a document viewer do not have.

### Homebrew: an own tap first, `homebrew-core` later

Two routes, and only one of them is available now.

**`homebrew-core`** gives the short `brew install termdoc`, but it applies a notability bar —
measured in stars, forks and watchers, with thresholds Homebrew documents in *Acceptable Formulae*
and revises over time. A newly published project does not clear it, and submitting anyway wastes a
maintainer's review. Revisit when the project has users.

**An own tap** needs nobody's approval — and **`marturojt/homebrew-tap` already exists**, carrying
a working `Formula/dapctl.rb`. termdoc adds a second formula to it rather than creating a tap:

```bash
brew install marturojt/tap/termdoc
```

The plan is therefore: this tap now, and migrate to `homebrew-core` when the notability bar is
cleared. A formula in a tap and a formula in core are nearly the same file, so this is not wasted
work.

### The prerequisite nobody thinks of first: tagged releases

A formula points at an immutable tarball plus its `sha256`, so nothing can be written until a
release exists. **This was the blocker and it is now cleared:** `v0.1.0` is tagged and released,
and `.github/workflows/release.yml` produces the per-platform archives plus `SHA256SUMS.txt` on
every `v*` tag. A future release needs no extra work for the formula to have URLs to point at.

### Ship prebuilt binaries, not a source build

The existing `dapctl.rb` already settles this question, and it settles it the right way: it ships
**prebuilt binaries** per platform — macOS universal, Linux x86_64, Linux aarch64 — with a `test do`
block, and `dapctl`'s `.github/workflows/release.yml` is what builds them. Copy that shape; it is
proven and it belongs to the same author.

The source-build alternative is a handful of lines:

```ruby
depends_on "rust" => :build

def install
  system "cargo", "install", *std_cargo_args(path: "crates/termdoc-cli")
end
```

**But this project has a specific reason to avoid it.** The workspace's `[profile.release]` sets
`lto = "fat"` and `codegen-units = 1`, and that profile *does* apply when building from the
repository tarball. Those settings buy a fast, small binary at the cost of a slow link, which is the
right trade for a release artifact built once in CI — and the wrong one for something every user
compiles on their own laptop while waiting.

So: source build to get the tap working, then bottles built in CI from tagged releases, which is
also what makes `brew install` feel instant. Bottles mean a release workflow producing macOS
arm64, macOS x86_64 and Linux artifacts.

### Keeping the formula current

A tap that lags the releases is worse than no tap. Whatever ships must bump the formula's `url`
and `sha256` automatically on each tagged release, from the same workflow that publishes to
crates.io. `dapctl` already does this; reuse its workflow rather than inventing one.

### Acceptance

```bash
brew install marturojt/tap/termdoc     # from the existing tap
brew audit --strict --new termdoc      # what homebrew-core would check
brew test termdoc                      # the formula's own test block runs
termdoc --version                      # matches the tagged release
```
