# 0003. Layering enforced by Cargo, not by convention

**Status:** Accepted

## Context

A reader that can see a backend will eventually emit ANSI, and then adding a backend means touching every reader.

## Decision

The allowed dependency edges are listed in DESIGN §3 and in `crates/termdoc-cli/tests/layering.rs`, which reads the manifests and fails if a new edge appears. Readers depend on `termdoc-core` only.

## Consequences

The compiler and a test hold the architecture. The price is that anything two readers share must live in `core` (`highlight`, `stream`, `timestamp`), and anything a reader needs from detection must arrive through `ReadContext` (the CSV delimiter, for instance), never through a new edge.
