# Architecture decision records

The decisions with real tension, and why they went the way they did. Numbering is the order they were written down, not the order they were taken. The ten that the original design promised are 0001-0010; later ones record what implementation taught.

| # | Decision | Status |
|---|---|---|
| 0001 | [An event stream instead of an AST](0001-event-stream-not-an-ast.md) | Accepted |
| 0002 | [Layout as a stage of its own](0002-layout-as-a-shared-stage.md) | Accepted |
| 0003 | [Layering enforced by Cargo, not by convention](0003-layering-by-the-crate-graph.md) | Accepted |
| 0004 | [Plugins launched lazily via manifests](0004-plugins-launched-lazily-via-manifests.md) | Accepted (not yet built: M4) |
| 0005 | [Plugins as subprocesses, not dynamic libraries](0005-subprocesses-over-dlopen.md) | Accepted (not yet built: M4) |
| 0006 | [A swappable PDF engine behind a trait](0006-swappable-pdf-engine.md) | Accepted (not yet built: M3) |
| 0007 | [YAML: neither `serde_yaml` nor `serde_yml`](0007-yaml-without-serde-yaml.md) | Accepted; extended by ADR 0011 |
| 0008 | [`SIGPIPE` reset to `SIG_DFL`](0008-sigpipe-reset-to-default.md) | Accepted |
| 0009 | [`DocumentReader`, not `Renderer`](0009-reader-not-renderer.md) | Accepted |
| 0010 | [Grapheme integrity outranks the width limit](0010-grapheme-integrity-over-width.md) | Accepted |
| 0011 | [Config and log formats are highlighted, not parsed](0011-highlight-rather-than-parse.md) | Accepted (M1) |
| 0012 | [Syntax highlighting maps scopes to roles, not themes to colours](0012-scopes-to-token-roles.md) | Accepted (M1) |
| 0013 | [Standard input is a feed, not a `Source`](0013-stdin-is-a-feed-not-a-source.md) | Accepted (M1) |
| 0014 | [A size ceiling degrades a document, it never refuses it](0014-ceilings-instead-of-refusal.md) | Accepted (M1) |

A decision is overturned by writing a new record that says so, and by updating `docs/DESIGN.md`; the old one stays, marked as superseded.
