# 0006. A swappable PDF engine behind a trait

**Status:** Accepted (not yet built: M3)

## Context

The Rust PDF ecosystem has no clear winner; the young, fast engine and the established one trade places.

## Decision

Define `trait PdfEngine`, implement it behind features, and treat `pdftotext`/`mutool` as an optional improvement when on `PATH`, never required.

## Consequences

The choice can change without touching readers. PDF text extraction stays the weakest part of the format list, and the documentation says so.
