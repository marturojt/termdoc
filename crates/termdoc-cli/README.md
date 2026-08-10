# termdoc

A universal document viewer for the terminal.

`termdoc` reads any document and renders it as well as the terminal allows. It never opens an
external application, and it degrades gracefully based on what the terminal can actually do.

It is not an editor. It is not a converter. It is not an IDE.

```bash
cargo install termdoc
```

```bash
termdoc README.md
termdoc data.csv
termdoc --explain odd.dat        # why that format, and which detection layers lost
termdoc --ascii --width 40 t.md

cat README.md | termdoc
git show HEAD:README.md | termdoc
```

In a pipe or a redirection it emits a clean, composable stream: only the document goes to stdout,
warnings go to stderr, and `termdoc huge.log | head -5` dies with signal 13 like `cat` does.

## Status

**Pre-1.0, and the roadmap is honest about it.** Today there are readers for **Markdown**, **plain
text** and **logs**.

Detection recognizes considerably more — JSON, YAML, TOML, XML, HTML, CSV, source code, PDF and the
Office/ZIP family — and decodes non-UTF-8 files correctly. Until each dedicated reader lands, a
recognized-but-unreadable text format is shown as plain text with a warning on stderr. Pass
`--strict` to turn that warning into an error.

Measured: **3.9 ms** startup, and **1.4 MB** of own memory while walking a 488 MB log.

The full roadmap is in [`docs/DESIGN.md`](https://github.com/marturojt/termdoc/blob/main/docs/DESIGN.md) §11.

## License

MIT OR Apache-2.0
