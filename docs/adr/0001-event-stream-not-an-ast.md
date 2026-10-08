# 0001. An event stream instead of an AST

**Status:** Accepted

## Context

A viewer must open a 10-million-line log or a 1M-row CSV and stay responsive, and it must not copy what it reads.

## Decision

The document model is a stream of `Event`s (`Start(Tag)` / `End(TagKind)` plus leaf events), in the style of `pulldown-cmark`, with `Cow<'a, str>` borrowing from the memory-mapped input.

## Consequences

Memory is flat: a 488 MB log is read with 1.4 MB of the process's own memory. Anything needing random access (a table of contents, column widths) materializes only the subtree it needs. A tree can be built from a stream and not the reverse, so the direction of the conversion is the point. Cost: readers cannot hand the layout a convenient tree. Revised once during M0 (inline containers needed `Start`/`End`, not a single `Inline` event); see DESIGN §2.2.
