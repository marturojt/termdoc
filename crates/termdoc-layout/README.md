# termdoc-layout

The layout engine of [`termdoc`](https://crates.io/crates/termdoc): it turns an event stream into
`Line`s.

Unicode-aware wrapping (grapheme clusters, CJK widths, combining marks, ZWJ sequences), table width
allocation, list markers, code blocks and theming. It takes a `Fidelity` as an input and picks the
glyph set and the color treatment from it.

Two invariants worth knowing: no line exceeds the width in display cells — except an indivisible
grapheme cluster wider than the line, because cluster integrity wins — and no line ends with
trailing spaces.

It depends on `termdoc-core` and `termdoc-term`, and sees neither readers nor backends.

**The API is unstable before 1.0.**

## License

MIT OR Apache-2.0
