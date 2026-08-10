# termdoc-term

Terminal capability detection for [`termdoc`](https://crates.io/crates/termdoc): width, color depth,
Unicode level, inline graphics and hyperlink support.

It resolves those into a `Fidelity`, which the layout and the backend take as an **input**. Graceful
degradation in `termdoc` is a value that gets passed down, not a chain of `if`s scattered through the
rendering code.

This crate depends on no other `termdoc` crate.

**The API is unstable before 1.0.**

## License

MIT OR Apache-2.0
