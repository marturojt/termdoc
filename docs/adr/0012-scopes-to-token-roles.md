# 0012. Syntax highlighting maps scopes to roles, not themes to colours

**Status:** Accepted (M1)

## Context

`syntect` can colour text with a TextMate theme, but a theme's colours are RGB. termdoc's colour model is the opposite: roles that the terminal's own palette answers to, degrading through the colour ladder.

## Decision

`syntect` is used for its parser only (pure-Rust `fancy-regex`, grammars from `two-face`). The scopes in force at each point of a line are mapped to `TokenRole`s, and `Theme::token_style` decides the look, exactly as for JSON keys. A `CodeBlock` the highlighter understands leaves as `Preformatted` with tokens; one it does not stays a `CodeBlock` with the theme's flat code colour.

## Consequences

Highlighted code obeys the user's palette and `NO_COLOR`. Cost, measured: grammars load in ~2 ms and only when code is highlighted, parsing runs at ~0.5 MB/s, each language costs ~10-18 MB, so highlighting is bounded to the first 512 KiB of a document and lines under 16 KiB. With no colour there is no highlighting at all, so a pipe never pays for it.
