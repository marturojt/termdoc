# 0005. Plugins as subprocesses, not dynamic libraries

**Status:** Accepted (not yet built: M4)

## Context

Rust has no stable ABI, and a crashing plugin must not take the viewer with it.

## Decision

Plugins are separate processes speaking a versioned protocol over stdio.

## Consequences

Fault isolation and a stable interface, paid for with serialization. Reader plugins are proxied behind the same `DocumentReader` trait, so the core has no 'is this a plugin?' branch.
