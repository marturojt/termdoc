# Release notes

A **fast**, terminal-native document viewer, with `inline code`
and a [link](https://github.com/marturojt/termdoc).

## Formats

- Markdown, plain text, logs, data files and source code render today
- HTML, PDF and the Office family are detected
  - dedicated readers are in development

| Format | Rendering | Milestone |
| ------ | :-------: | --------- |
| Markdown | yes | M0 |
| Logs | yes | M0 |
| PDF | detected | M3 |

```rust
fn main() {
    println!("hello");
}
```

> [!NOTE]
> Streaming by default: a 488 MB log costs 1.4 MB of memory.
