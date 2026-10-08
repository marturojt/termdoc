# 0013. Standard input is a feed, not a `Source`

**Status:** Accepted (M1)

## Context

`Events<'a>` borrows from a `&Source`, and a buffer that is still growing cannot be borrowed from. Reading stdin to its end first meant `kubectl logs -f | termdoc` showed nothing and `yes | termdoc | head` never finished.

## Decision

A thread reads stdin into a bounded queue. Detection looks at the first chunk; a reader that can read line by line (`streams_input`) takes a `LineStream` and emits owned lines, and every other reader gets stdin read to its end as before. Output is flushed when the stream is starved — nothing complete is buffered or queued and the input has not ended — not after every line.

## Consequences

122 MB piped in costs 2.8 MB instead of 122 MB, and a live line appears when it is written. No existing reader changed. Limits, all documented: three timestamped lines are needed to detect a log (`--from log` overrides), the encoding is decided from the first chunk, a line over 16 MiB is split.
