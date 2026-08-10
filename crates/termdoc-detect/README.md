# termdoc-detect

Layered format and encoding detection for [`termdoc`](https://crates.io/crates/termdoc).

Each layer — magic bytes, extension, structure, delimiter sniffing — proposes a format with a
confidence and a human-readable reason, and the layers can disagree. `termdoc --explain` prints
every opinion, including the losing ones, which is what makes a misdetection debuggable instead of
mysterious.

It also resolves the character encoding, so that **no reader ever decides how bytes become text**.

Detection answers *what a document is*, never *how it looks*: this crate depends only on
`termdoc-core` and cannot see a layout or a backend.

**The API is unstable before 1.0.**

## License

MIT OR Apache-2.0
