#!/usr/bin/env python3
"""termdoc's performance gates (docs/DESIGN.md §8).

The budgets are tests, not aspirations: this script exits non-zero if any of them is
violated, so a performance regression breaks CI the same way a failing test does.

The thresholds are configurable through the environment, because a CI machine is slower
and noisier than a laptop, and an over-tight threshold would produce intermittent failures
— which are worse than having no gate at all:

    TERMDOC_MAX_STARTUP_MS     startup with a small Markdown file   (default 10)
    TERMDOC_MAX_OWN_MEM_MB     own memory with a large log          (default 50)
    TERMDOC_CORPUS_LINES       lines in the generated log           (default 300000)

Usage:  python3 scripts/perf-gate.py [path-to-binary]
"""

from __future__ import annotations

import os
import platform
import resource
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BINARY = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "target/release/termdoc"

MAX_STARTUP_MS = float(os.environ.get("TERMDOC_MAX_STARTUP_MS", "10"))
MAX_OWN_MEM_MB = float(os.environ.get("TERMDOC_MAX_OWN_MEM_MB", "50"))
CORPUS_LINES = int(os.environ.get("TERMDOC_CORPUS_LINES", "300000"))

failures: list[str] = []


def ok(name: str, detail: str) -> None:
    print(f"  \033[32mOK\033[0m    {name:<44} {detail}")


def fail(name: str, detail: str) -> None:
    print(f"  \033[31mFAIL\033[0m  {name:<44} {detail}")
    failures.append(name)


def ensure_log() -> Path:
    """The large log is not versioned because of its size: generate it if absent."""
    path = ROOT / "corpus/huge.log"
    if path.exists():
        return path
    print(f"  generating {path.name} with {CORPUS_LINES} lines...")
    path.parent.mkdir(exist_ok=True)
    with path.open("w") as f:
        for i in range(CORPUS_LINES):
            f.write(
                f"2026-08-10T12:00:{i % 60:02d}Z INFO  worker[{i % 8}] "
                f"processing record number {i} with enough filler "
                f"to make the line realistic\n"
            )
    return path


def gate_startup() -> None:
    """Startup has to be near-instant: that is what allows using it like `cat`."""
    doc = ROOT / "corpus/basic.md"
    # Warm-up rounds, so we do not measure the binary's first page faults.
    for _ in range(5):
        subprocess.run([BINARY, doc], capture_output=True)

    samples = []
    for _ in range(40):
        t = time.perf_counter()
        subprocess.run([BINARY, doc], capture_output=True)
        samples.append((time.perf_counter() - t) * 1000)
    samples.sort()
    median = samples[len(samples) // 2]

    detail = f"median {median:.1f} ms (limit {MAX_STARTUP_MS:.0f} ms)"
    (ok if median <= MAX_STARTUP_MS else fail)("startup", detail)


def own_memory_mb(args: list) -> float | None:
    """The child process's anonymous memory, in MB.

    Own memory is measured rather than RSS, and deliberately so. With `mmap`, walking the
    document leaves its pages resident and RSS ends up tracking the file size; but those
    are clean, file-backed pages that the kernel reclaims instantly and that are not memory
    the process owns. See docs/DESIGN.md §8.
    """
    system = platform.system()
    if system == "Darwin":
        # macOS reports "peak memory footprint", which is exactly the anonymous memory.
        r = subprocess.run(
            ["/usr/bin/time", "-l", *map(str, args)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
        )
        for line in r.stderr.splitlines():
            if "peak memory footprint" in line:
                return int(line.split()[0]) / 1048576
        return None

    if system == "Linux":
        # Linux has no direct equivalent, so the child's own maximum is read instead.
        before = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
        subprocess.run(list(map(str, args)), capture_output=True)
        after = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
        # On Linux ru_maxrss is in KiB and *includes* mapped pages, so this figure is a
        # pessimistic ceiling rather than the own memory. It is reported as such.
        return max(after, before) / 1024

    return None


def gate_memory() -> None:
    log = ensure_log()
    mb = own_memory_mb([BINARY, log])

    if mb is None:
        print(f"  \033[33mSKIP\033[0m  own memory — no method available on {platform.system()}")
        return

    size_mb = log.stat().st_size / 1048576
    detail = f"{mb:.2f} MB with {size_mb:.0f} MB of input (limit {MAX_OWN_MEM_MB:.0f} MB)"
    if platform.system() == "Linux":
        # The Linux figure includes mapped pages: only check that it does not explode.
        detail += " [pessimistic ceiling]"
        limit = max(MAX_OWN_MEM_MB, size_mb * 1.2 + 20)
    else:
        limit = MAX_OWN_MEM_MB
    (ok if mb <= limit else fail)("own memory", detail)


def gate_lazy_output() -> None:
    """`termdoc huge.log | head -5` must not read the whole file.

    This gate measures two things at once: that the reader is lazy, and that SIGPIPE is
    handled properly — were the `SIG_DFL` reset missing, the process would end in a panic.
    """
    log = ensure_log()

    t = time.perf_counter()
    p1 = subprocess.Popen([BINARY, log], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    p2 = subprocess.Popen(["head", "-5"], stdin=p1.stdout, stdout=subprocess.PIPE)
    p1.stdout.close()
    output = p2.communicate()[0]
    p1.wait()
    err = p1.stderr.read().decode(errors="replace")
    elapsed = (time.perf_counter() - t) * 1000

    lines = len(output.splitlines())
    # Reading the whole file would cost an order of magnitude more; the limit is generous so
    # it does not depend on disk speed.
    limit_ms = 250.0

    if lines != 5:
        fail("lazy output", f"expected 5 lines, got {lines}")
    elif err.strip():
        fail("lazy output", f"stderr was not clean: {err.strip()[:80]}")
    elif elapsed > limit_ms:
        fail("lazy output", f"{elapsed:.0f} ms (limit {limit_ms:.0f} ms)")
    else:
        ok(
            "lazy output (| head -5)",
            f"{elapsed:.0f} ms, rc={p1.returncode}, stderr clean",
        )


def main() -> int:
    if not BINARY.exists():
        print(f"{BINARY} does not exist; run `cargo build --release` first", file=sys.stderr)
        return 2

    print(f"performance gates — {platform.system()} — {BINARY}")
    gate_startup()
    gate_memory()
    if platform.system() != "Windows":
        gate_lazy_output()
    else:
        print("  \033[33mSKIP\033[0m  lazy output — Windows has no SIGPIPE")

    if failures:
        print(f"\n{len(failures)} gate(s) violated: {', '.join(failures)}", file=sys.stderr)
        return 1
    print("\nall gates within budget")
    return 0


if __name__ == "__main__":
    sys.exit(main())
