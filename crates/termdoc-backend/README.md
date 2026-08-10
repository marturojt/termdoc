# termdoc-backend

The output backends of [`termdoc`](https://crates.io/crates/termdoc): ANSI and plain text.

A backend consumes `Line`s and writes bytes. It tracks the current style as state, so redundant SGR
sequences never reach the terminal, and it guarantees that **every line ends with no style active**
and that with `ColorDepth::None` not one escape byte is emitted.

It depends on `termdoc-core` and `termdoc-term`, and notably **not** on the layout engine: `Line`
lives in core precisely so this edge does not need to exist.

**The API is unstable before 1.0.**

## License

MIT OR Apache-2.0
