# 0010. Grapheme integrity outranks the width limit

**Status:** Accepted

## Context

A ZWJ emoji is two cells wide and indivisible; in a one-column terminal the two rules collide.

## Decision

Never split a grapheme cluster, even when that makes a line overflow. The two documented exceptions to 'no line exceeds the width' are an indivisible cluster wider than the line, and the TSV table rung.

## Consequences

Splitting a cluster draws visible garbage and does not even fix the overflow. A property test generates CJK, combining marks, ZWJ sequences and flags to hold the rule.
