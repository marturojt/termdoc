# 0002. Layout as a stage of its own

**Status:** Accepted

## Context

Wrapping, tables, lists and Unicode widths are where typographic quality lives, and every format needs them.

## Decision

Readers describe a document; one layout engine (`termdoc-layout`) turns events into `Line`s; backends only paint lines. `Line` lives in `core` so the backend does not depend on the layout.

## Consequences

Every reader gets tables that fit and text that wraps by grapheme cluster for free, and fixes land once. A reader cannot influence appearance; if it needs the terminal width or a colour, the answer belongs in the layout or the theme.
