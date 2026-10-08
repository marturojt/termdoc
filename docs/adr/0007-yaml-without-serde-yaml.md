# 0007. YAML: neither `serde_yaml` nor `serde_yml`

**Status:** Accepted; extended by ADR 0011

## Context

`serde_yaml` is deprecated and its fork `serde_yml` was archived over unsoundness.

## Decision

Use neither. `yaml-rust2` was the recommended replacement. In the event, the YAML reader uses no parser at all (ADR 0011).

## Consequences

No unmaintained dependency in the tree. `yaml-rust2` remains the answer if YAML *validation* is ever wanted.
