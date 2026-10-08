# 0014. A size ceiling degrades a document, it never refuses it

**Status:** Accepted (M1)

## Context

Three places cannot be both complete and bounded: JSON is parsed into a tree, a CSV table needs every row to size its columns, and syntax highlighting is slow.

## Decision

Each has a ceiling derived from a measurement of the whole pipeline, not the part (JSON 512 KiB; CSV 25,000 cells; highlighting 512 KiB). Past it the document is shown as it is, lazily, with one warning, and `--strict` turns the warning into a failure.

## Consequences

Nothing is lost and memory stays bounded. The recurring lesson, recorded in each module header: measuring the parser alone gave a ceiling four times too generous.
