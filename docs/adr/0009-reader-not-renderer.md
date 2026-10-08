# 0009. `DocumentReader`, not `Renderer`

**Status:** Accepted

## Context

'Renderer' suggests a thing that paints, and the first design used it for the thing that only produces the model.

## Decision

The trait that turns bytes into events is `DocumentReader`. 'Render' is reserved for layout plus backend.

## Consequences

The vocabulary matches the layering: readers describe, layout lays out, backends paint.
