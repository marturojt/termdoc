# termdoc-read-text

The plain-text, log and Markdown readers for [`termdoc`](https://crates.io/crates/termdoc).

Both are streaming: they walk the source line by line and borrow from it, which is what makes
`termdoc huge.log | head -5` read a few pages instead of the whole file.

A reader **describes** a document; it does not decide how it looks. If a reader needed the terminal
width or a color, that would belong in the layout or the theme instead. This crate therefore depends
only on `termdoc-core` — it cannot see a backend, so it cannot emit ANSI.

**The API is unstable before 1.0.**

## License

MIT OR Apache-2.0
