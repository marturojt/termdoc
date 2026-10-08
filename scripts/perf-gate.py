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
# What colour-forced highlighting may add to a plain run: for one language, and for a document
# with no code at all.
MAX_HIGHLIGHT_MEM_MB = float(os.environ.get("TERMDOC_MAX_HIGHLIGHT_MEM_MB", "30"))
MAX_NO_CODE_MEM_MB = float(os.environ.get("TERMDOC_MAX_NO_CODE_MEM_MB", "3"))
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


# Runs a command and prints the peak resident size of that one child, in the OS's own unit.
_PEAK_RSS_SNIPPET = (
    "import resource, subprocess, sys;"
    "subprocess.run(sys.argv[1:], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL);"
    "print(resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss)"
)


def own_memory_mb(args: list, stdin_path: Path | None = None) -> float | None:
    """The child process's anonymous memory, in MB.

    Own memory is measured rather than RSS, and deliberately so. With `mmap`, walking the
    document leaves its pages resident and RSS ends up tracking the file size; but those
    are clean, file-backed pages that the kernel reclaims instantly and that are not memory
    the process owns. See docs/DESIGN.md §8.
    """
    system = platform.system()
    stdin = open(stdin_path, "rb") if stdin_path else None
    try:
        return _own_memory_mb(system, args, stdin)
    finally:
        if stdin:
            stdin.close()


def _own_memory_mb(system: str, args: list, stdin) -> float | None:
    if system == "Darwin":
        # macOS reports "peak memory footprint", which is exactly the anonymous memory.
        r = subprocess.run(
            ["/usr/bin/time", "-l", *map(str, args)],
            stdin=stdin,
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
        #
        # `RUSAGE_CHILDREN` is the maximum over *every* child this process has waited for, so
        # reading it here would report the largest process of the whole run, not this one. A fresh
        # interpreter per measurement has exactly one child, which makes the figure this child's.
        r = subprocess.run(
            [sys.executable, "-c", _PEAK_RSS_SNIPPET, *map(str, args)],
            stdin=stdin,
            capture_output=True,
            text=True,
        )
        if r.returncode != 0 or not r.stdout.strip():
            return None
        # On Linux ru_maxrss is in KiB and *includes* mapped pages, so this figure is a
        # pessimistic ceiling rather than the own memory. It is reported as such.
        return int(r.stdout.strip()) / 1024

    return None


def gate_highlight() -> None:
    """Syntax highlighting is paid for only where there is code.

    Both checks run with colour forced (a pipe skips highlighting, which would make the gate
    measure nothing), and both are *relative to the same binary on a plain document*. That
    cancels what differs between platforms, chiefly that Linux reports resident size, which
    includes the executable's own pages, where macOS reports anonymous memory.

    A document with no code must not load a grammar, which is what keeps highlighting out of
    everyone else's startup. And one language must stay within what was measured: every grammar
    costs roughly 10-18 MB (`engine.rs` in termdoc-read-code), and a regression there, say a
    feature flag that pulls in a heavier regex engine, should be a failure rather than a surprise.
    """
    baseline = own_memory_mb([BINARY, ROOT / "corpus/plain.txt"])
    if baseline is None:
        print(f"  \033[33mSKIP\033[0m  highlighting — no method available on {platform.system()}")
        return

    cases = [
        ("highlighting: no code, no cost", ROOT / "corpus/plain.txt", MAX_NO_CODE_MEM_MB),
        ("highlighting: one language", ROOT / "corpus/code.rs", MAX_HIGHLIGHT_MEM_MB),
    ]
    for name, doc, limit in cases:
        mb = own_memory_mb([BINARY, "--color", "always", doc])
        if mb is None:
            print(f"  \033[33mSKIP\033[0m  {name} — no method available on {platform.system()}")
            continue
        extra = mb - baseline
        detail = f"+{extra:.1f} MB over a plain run of {baseline:.1f} MB (limit +{limit:.0f} MB)"
        (ok if extra <= limit else fail)(name, detail)


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


def gate_stdin() -> None:
    """Standard input is read as it arrives, and nothing already shown is kept.

    Two things, one per failure. Piping a huge file through must cost what reading it as a file
    does: `Source::from_stdin` used to hold every byte, so 122 MB in meant 122 MB of memory. And
    an input that never ends must not stop the first lines from appearing: `yes | termdoc |
    head` used to read forever and print nothing, which is what the process being *fast* here
    proves, since "read forever" has no finishing time to be fast at.
    """
    log = ensure_log()
    mb = own_memory_mb([BINARY], stdin_path=log)
    if mb is None:
        print(f"  \033[33mSKIP\033[0m  stdin memory — no method available on {platform.system()}")
    else:
        size_mb = log.stat().st_size / 1048576
        limit = MAX_OWN_MEM_MB
        detail = f"{mb:.2f} MB with {size_mb:.0f} MB piped in (limit {limit:.0f} MB)"
        if platform.system() == "Linux":
            detail += " [pessimistic ceiling]"
        (ok if mb <= limit else fail)("stdin memory", detail)

    if platform.system() == "Windows":
        print("  \033[33mSKIP\033[0m  endless stdin — no `yes` on Windows")
        return
    t = time.perf_counter()
    yes = subprocess.Popen(
        ["yes", "2026-08-10 12:00:00 INFO an endless stream"], stdout=subprocess.PIPE
    )
    term = subprocess.Popen(
        [BINARY], stdin=yes.stdout, stdout=subprocess.PIPE, stderr=subprocess.PIPE
    )
    head = subprocess.Popen(["head", "-5"], stdin=term.stdout, stdout=subprocess.PIPE)
    yes.stdout.close()
    term.stdout.close()
    try:
        output = head.communicate(timeout=10)[0]
    except subprocess.TimeoutExpired:
        head.kill()
        output = b""
    elapsed = (time.perf_counter() - t) * 1000
    for proc in (term, yes):
        proc.kill()
        proc.wait()
    lines = len(output.splitlines())
    if lines != 5:
        fail("endless stdin (| head -5)", f"expected 5 lines in 10 s, got {lines}")
    elif elapsed > 1000.0:
        fail("endless stdin (| head -5)", f"{elapsed:.0f} ms (limit 1000 ms)")
    else:
        ok("endless stdin (| head -5)", f"{elapsed:.0f} ms")


def main() -> int:
    if not BINARY.exists():
        print(f"{BINARY} does not exist; run `cargo build --release` first", file=sys.stderr)
        return 2

    print(f"performance gates — {platform.system()} — {BINARY}")
    gate_startup()
    gate_highlight()
    gate_memory()
    gate_stdin()
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
