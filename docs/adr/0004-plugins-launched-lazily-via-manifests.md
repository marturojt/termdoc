# 0004. Plugins launched lazily via manifests

**Status:** Accepted (not yet built: M4)

## Context

Instant startup is a feature (`termdoc` is used like `cat`), and a plugin ecosystem must not cost it.

## Decision

Plugins ship a `plugin.toml` declaring formats, extensions and magic bytes. The core reads only those manifests, caches a merged index, and starts a plugin process only when a document needs it.

## Consequences

Startup does not depend on how many plugins are installed. Not implemented until M4; recorded so the detection layers are built to accept externally declared formats.
