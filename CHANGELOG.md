# Changelog

## 0.2.0

M1 — data and code. termdoc now reads nine formats and shows them the way they deserve, and reads
a pipe as it arrives.

### New formats

- **JSON** is re-indented into a readable tree, with keys, strings, numbers and punctuation
  coloured apart. Above 512 KiB it is shown as it is, with a warning.
- **YAML**, **TOML** and **XML** are syntax-highlighted with their comments kept: the text is the
  file on disk, byte for byte, only coloured. Anchors, aliases, multi-line strings, CDATA and
  array-of-tables are all understood.
- **CSV** and **TSV** become tables, with numeric columns right-aligned, quoted fields and embedded
  newlines handled, and the delimiter detected. `--delimiter` overrides it and `--csv-header
  auto|yes|no` decides whether the first row is a header. Beyond 25,000 cells the table closes and
  the remaining rows follow verbatim.
- **Source code** is highlighted with over 200 grammars, and so are fenced code blocks in Markdown.
  The colours come from your terminal's own palette, not from a fixed theme.
- **Logs** show the timestamp, the level and `key=value` keys, so a log can be read down its
  levels.

### Pipes

- Standard input is **read as it arrives**. `kubectl logs -f pod | termdoc` shows each line when it
  is written, `yes | termdoc | head` answers at once, and 122 MB piped in costs 2.8 MB of memory
  where it used to cost 122 MB.

### Under the hood

- Eleven new token roles — five for code, six for logs — and `Theme` decides how each looks.
- New crates `termdoc-read-data` and `termdoc-read-code`. Shared line-by-line machinery
  (`highlight`), the stdin feed (`stream`) and the timestamp parser (`timestamp`) live in
  `termdoc-core`.
- Performance gates now also pin the cost of highlighting and of reading stdin.
- Architecture decision records in `docs/adr/`.

### Limits worth knowing

- Three timestamped lines are needed for detection to call something a log; `--from log` forces it.
- Each highlighted language costs roughly 10-18 MB of memory while it is loaded, and highlighting
  stops after 512 KiB of one document (with a warning).
- XML is not re-indented: a one-line response stays one line.
- HTML is recognised but shown as plain text until M3.

## 0.1.0

M0 — Markdown, plain text and logs; format detection; the layout engine, tables and the colour
ladder; pipes that behave.
