#!/usr/bin/env python3
"""Points the Homebrew formula at a release.

    python3 scripts/bump-tap.py 0.2.1             # rewrite, commit and push marturojt/homebrew-tap
    python3 scripts/bump-tap.py 0.2.1 --dry-run   # show what would change, touch nothing

The formula reads the checksums from the release's own `SHA256SUMS.txt`, so nothing here is
typed by hand and a typo cannot pin the wrong archive. It writes the whole file from one
template, which is what makes `--dry-run` against the current version a check in itself: it must
come out identical to what is already published.

Why a script and not a step in the release workflow: pushing to the tap needs a token with write
access to *another* repository, which is a secret someone has to create. This is the same bump,
one command, until that exists (docs/HANDOFF.md §11).

Needs `gh` (authenticated) and `git` with push access to the tap.
"""

import subprocess
import sys
import tempfile
from pathlib import Path

REPO = "marturojt/termdoc"
TAP = "git@github.com:marturojt/homebrew-tap.git"

TARGETS = {
    "macos": "universal-apple-darwin",
    "linux_intel": "x86_64-unknown-linux-gnu",
    "linux_arm": "aarch64-unknown-linux-gnu",
}

TEMPLATE = '''class Termdoc < Formula
  desc "Universal document viewer for the terminal"
  homepage "https://termdoc.app"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    url "{macos_url}"
    sha256 "{macos_sha}"
  end

  on_linux do
    on_intel do
      url "{linux_intel_url}"
      sha256 "{linux_intel_sha}"
    end

    on_arm do
      url "{linux_arm_url}"
      sha256 "{linux_arm_sha}"
    end
  end

  def install
    bin.install "termdoc"
  end

  test do
    assert_match "termdoc #{{version}}", shell_output("#{{bin}}/termdoc --version")

    # The binary has to actually read documents, not just print its version.
    (testpath/"t.md").write("# hello\\n\\nsome **text**\\n")
    assert_match "some text", shell_output("#{{bin}}/termdoc #{{testpath}}/t.md")
    assert_match "yaml", shell_output("#{{bin}}/termdoc --formats")
  end
end
'''


def run(*cmd, cwd=None, capture=True):
    r = subprocess.run(cmd, cwd=cwd, text=True, capture_output=capture)
    if r.returncode != 0:
        sys.exit(f"{' '.join(cmd)} failed:\n{r.stderr or r.stdout}")
    return r.stdout


def checksums(version: str) -> dict[str, str]:
    """Archive name -> sha256, from the release's SHA256SUMS.txt."""
    with tempfile.TemporaryDirectory() as tmp:
        run("gh", "release", "download", f"v{version}", "-R", REPO,
            "-p", "SHA256SUMS.txt", "-D", tmp)
        lines = (Path(tmp) / "SHA256SUMS.txt").read_text().splitlines()
    return {name: digest for digest, name in (line.split() for line in lines if line.strip())}


def formula(version: str, sums: dict[str, str]) -> str:
    fields = {}
    for key, target in TARGETS.items():
        archive = f"termdoc-v{version}-{target}.tar.gz"
        if archive not in sums:
            sys.exit(f"{archive} is not in the release's SHA256SUMS.txt; is v{version} fully built?")
        fields[f"{key}_url"] = f"https://github.com/{REPO}/releases/download/v{version}/{archive}"
        fields[f"{key}_sha"] = sums[archive]
    return TEMPLATE.format(**fields)


def main() -> int:
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    dry = "--dry-run" in sys.argv
    if len(args) != 1:
        print(__doc__)
        return 2
    version = args[0].lstrip("v")

    text = formula(version, checksums(version))
    with tempfile.TemporaryDirectory() as tmp:
        run("git", "clone", "-q", TAP, tmp)
        path = Path(tmp) / "Formula" / "termdoc.rb"
        path.write_text(text)
        diff = run("git", "diff", "--stat", "--patch", cwd=tmp)
        if not diff.strip():
            print(f"Formula/termdoc.rb already points at v{version}; nothing to do.")
            return 0
        print(diff)
        if dry:
            print("--dry-run: nothing was committed.")
            return 0
        run("git", "add", "Formula/termdoc.rb", cwd=tmp)
        run("git", "commit", "-q", "-m", f"termdoc {version}", cwd=tmp)
        run("git", "push", "-q", "origin", "main", cwd=tmp)
        print(f"Pushed termdoc {version}. The tap's CI installs it on macOS and Linux; watch it with:")
        print("  gh run watch -R marturojt/homebrew-tap")
    return 0


if __name__ == "__main__":
    sys.exit(main())
