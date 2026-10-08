# Code blocks

A highlighted Python block:

```python
def greet(name: str = "world") -> str:
    """Say hello."""
    return f"hello, {name}!"  # friendly


@decorator
class Greeter(Base):
    count = 3
```

JavaScript, which has template strings and arrow functions:

```js
const add = (a, b) => a + b;
const msg = `sum: ${add(1, 2)}`;
```

A shell block with a pipeline:

```bash
for f in *.log; do grep -c ERROR "$f" | sort -n; done
```

A block with an unknown language keeps its flat colour:

```nonsense-lang
fn this_is_not_highlighted() {}
```

So does a block with no language at all:

```
plain block, no language
```

And `text` is deliberately not a language:

```text
fn nor_is_this() {}
```
