# Release notes

A **fast**, terminal-native document viewer, with `inline code`
and a [link](https://github.com/marturojt/termdoc).

## Formats

- Markdown, plain text and logs render today
- JSON, YAML, TOML, CSV are detected
  - dedicated readers are in development

| Format | Rendering | Milestone |
| ------ | :-------: | --------- |
| Markdown | yes | M0 |
| Logs | yes | M0 |
| CSV | detected | M1 |

```rust
fn main() {
    println!("hello");
}
```

> [!NOTE]
> Streaming by default: a 488 MB log costs 1.4 MB of memory.
