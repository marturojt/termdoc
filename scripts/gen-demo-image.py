#!/usr/bin/env python3
"""Renders a real `termdoc` run into docs/assets/demo.png.

The image in the README is a screenshot of actual output, not an illustration:
this script runs the release binary, translates the ANSI it emits into HTML, and
photographs that with headless Chrome. If the renderer's colours or glyphs
change, re-run it and the asset changes with them.

    cargo build --release && python3 scripts/gen-demo-image.py

Requires headless Chrome. Exits rather than guessing if termdoc emits a style it
does not know how to map, so the asset can never quietly misrepresent the tool.
"""

import html
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BINARY = ROOT / "target" / "release" / "termdoc"
DOC = ROOT / "scripts" / "demo-doc.md"
OUT = ROOT / "docs" / "assets" / "demo.png"
WIDTH_COLS = 62

CHROME_CANDIDATES = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "google-chrome",
    "chromium",
    "chromium-browser",
]

# Terminal palette for the screenshot. These are the colours a terminal would
# assign to the ANSI codes termdoc emits; the tool itself never picks RGB.
PALETTE = {
    "30": "#414868", "31": "#f7768e", "32": "#9ece6a", "33": "#e0af68",
    "34": "#565f89", "35": "#bb9af7", "36": "#7dcfff", "37": "#a9b1d6",
    "90": "#6b7394", "91": "#ff7a93", "92": "#b9f27c", "93": "#e0af68",
    "94": "#7aa2f7", "95": "#c8a6fa", "96": "#7dcfff", "97": "#c0caf5",
}
ATTRS = {"1": "font-weight:700", "2": "opacity:.6",
         "3": "font-style:italic", "4": "text-decoration:underline"}

SGR = re.compile(r"\x1b\[([0-9;]*)m")


def find_chrome():
    for c in CHROME_CANDIDATES:
        if Path(c).exists() or shutil.which(c):
            return c
    sys.exit("no headless Chrome found; install Chrome or Chromium and retry")


def ansi_to_html(text):
    out, styles, pos, open_span = [], [], 0, False

    def close():
        nonlocal open_span
        if open_span:
            out.append("</span>")
            open_span = False

    for m in SGR.finditer(text):
        chunk = text[pos:m.start()]
        if chunk:
            out.append(html.escape(chunk))
        pos = m.end()
        params = [p for p in m.group(1).split(";") if p] or ["0"]
        close()
        for p in params:
            if p == "0":
                styles = []
            elif p in ATTRS:
                styles.append(ATTRS[p])
            elif p in PALETTE:
                styles.append(f"color:{PALETTE[p]}")
            else:
                sys.exit(f"unmapped SGR parameter {p!r}; the renderer grew a style this script does not know")
        if styles:
            out.append('<span style="%s">' % ";".join(styles))
            open_span = True

    tail = text[pos:]
    if tail:
        out.append(html.escape(tail))
    close()
    return "".join(out).rstrip("\n")


def main():
    if not BINARY.exists():
        sys.exit(f"{BINARY.relative_to(ROOT)} not found — run `cargo build --release` first")

    proc = subprocess.run(
        [str(BINARY), "--color", "always", "--width", str(WIDTH_COLS), str(DOC)],
        capture_output=True, text=True, check=True,
    )
    body = ansi_to_html(proc.stdout)

    page = f"""<!doctype html><meta charset="utf-8"><style>
      html {{ background: #11131a; }}
      body {{ margin: 0; padding: 28px 30px; }}
      pre {{
        margin: 0;
        font: 14px/1.25 Menlo, ui-monospace, "DejaVu Sans Mono", monospace;
        color: #c0caf5;
        white-space: pre;
      }}
      .chrome {{ margin-bottom: 18px; }}
      .dot {{ display:inline-block; width:11px; height:11px; border-radius:50%;
              margin-right:7px; vertical-align:middle; }}
      .t {{ color:#6b7394; font:12px ui-monospace,monospace; margin-left:8px; }}
    </style>
    <div class="chrome">
      <span class="dot" style="background:#ff5f56"></span>
      <span class="dot" style="background:#ffbd2e"></span>
      <span class="dot" style="background:#27c93f"></span>
      <span class="t">termdoc release-notes.md</span>
    </div>
    <pre>{body}</pre>"""

    OUT.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        src = Path(tmp) / "demo.html"
        src.write_text(page)
        lines = proc.stdout.count("\n") + 1
        height = 28 * 2 + 18 + 20 + int(lines * 14 * 1.25)
        subprocess.run(
            [find_chrome(), "--headless", "--disable-gpu", "--hide-scrollbars",
             "--force-device-scale-factor=2",
             f"--screenshot={OUT}",
             f"--window-size={WIDTH_COLS * 9 + 60},{height}",
             src.as_uri()],
            capture_output=True, check=True,
        )
    print(f"wrote {OUT.relative_to(ROOT)} ({OUT.stat().st_size // 1024} KB)")


if __name__ == "__main__":
    main()
