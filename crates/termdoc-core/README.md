# termdoc-core

The shared vocabulary of [`termdoc`](https://crates.io/crates/termdoc): the document model, the
pipeline traits and the input sources.

The document is an **event stream**, not a tree — `Event::Start(Tag)` / `End(TagKind)` plus leaf
events, borrowing from the source with `Cow<'a, str>`. That is what lets a 488 MB log be read with
1.4 MB of own memory. Anything that needs random access is materialized locally, never the whole
document.

This crate depends on no other `termdoc` crate: it is the root of the graph, and the layering is
enforced by Cargo rather than by discipline.

**The API is unstable before 1.0.** The document model and the plugin protocol are deliberately
not frozen yet; see [`docs/DESIGN.md`](https://github.com/marturojt/termdoc/blob/main/docs/DESIGN.md)
§11.

## License

MIT OR Apache-2.0
