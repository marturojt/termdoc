#!/usr/bin/env bash
# Generates the pathological corpus that is too large to keep in version control.
#
# These files are what measure whether termdoc behaves like a Unix utility: startup time,
# flat memory, and lazy output. See docs/DESIGN.md §8.
set -euo pipefail

cd "$(dirname "$0")/.."
mkdir -p corpus

lines="${1:-1000000}"

echo "generating corpus/huge.log with $lines lines..."
python3 - "$lines" <<'EOF'
import sys
n = int(sys.argv[1])
with open("corpus/huge.log", "w") as f:
    for i in range(n):
        f.write(
            f"2026-08-10T12:00:{i % 60:02d}Z INFO  worker[{i % 8}] "
            f"processing record number {i} with enough filler "
            f"to make the line realistic\n"
        )
EOF

echo "generating corpus/one-huge-line.txt (1 MB with no breaks)..."
python3 -c "open('corpus/one-huge-line.txt','w').write('x'*1_000_000 + '\n')"

ls -lh corpus/huge.log corpus/one-huge-line.txt
