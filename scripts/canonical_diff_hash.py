#!/usr/bin/env python3
"""
Canonical diff hash for a cargo worktree — the content-hash side of the
content-addressed build cache (Row 10 / Wave 0).

Computes a sha256 over the *content* of a worktree's working state, in
a form that's stable across:

  - the worktree's absolute path
  - line-ending style (CRLF / LF)
  - the per-file blob shas git embeds in `index <hex>..<hex>` lines
    (these vary by repo init order)
  - hunk ordering across files

Two worktrees that diverge from the same base commit and end up with
identical file content will hash to identical `diff_sha` values. That
is the cache key under which artifacts and CI results are stored;
shared keys ⇒ shared cache hits ⇒ cross-worktree, cross-machine reuse.

The Rust reproducibility flags in `src-tauri/.cargo/config.toml` are
the companion to this script: with /Brepro (Windows) /
`-Wl,--build-id=none` (Linux), the *artifact* sha is also stable, so
cache entries are not just keyed identically but also serve byte-
identical bytes.

Production cache key (composed by the supervisor build pool):

    key = sha256(base_sha || ":"
              || diff_sha || ":"
              || rustc_version || ":"
              || cargo_profile || ":"
              || target_triple)

This script emits `base_sha` and `diff_sha`. The remaining dimensions
are environment values the caller fills in.

Usage
-----

CLI:

    # Hash the dirty state in a worktree against its first commit:
    $ canonical_diff_hash.py /path/to/worktree
    base_sha=dc9903b90b2b7e9911bf791a63ca834d81dd44c3
    diff_sha=281355765decbeee7fba4c8e0f66d99cf9284d859301d717ee49f2013709641f
    canonical_bytes=420

    # Hash against a specific base ref (e.g. origin/main):
    $ canonical_diff_hash.py /path/to/worktree --base origin/main

    # See the canonical diff body that was hashed (for debugging key
    # mismatches across worktrees that should agree):
    $ canonical_diff_hash.py /path/to/worktree --show-canonical

Programmatic:

    from canonical_diff_hash import compute, canonicalize_diff
    base_sha, diff_sha, n_bytes = compute("/path/to/worktree",
                                          base="origin/main")

History / provenance
--------------------

Promoted from `tmp_spike_bazel_remote/scripts/canonical_diff_hash.py`
on 2026-05-14, after the Phase-0 spike confirmed the recipe makes
identical content at different paths produce identical hashes. See
memory `proj_canonical_diff_hash_recipe` for the empirical proof + the
production key shape; do not deviate from the recipe here without
updating that memory.
"""
from __future__ import annotations

import argparse
import hashlib
import os
import subprocess
import sys
from typing import Optional, Tuple


def _run(cmd: list, cwd: str) -> str:
    proc = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, check=True)
    return proc.stdout


def canonicalize_diff(diff: str, worktree_abs_path: str) -> str:
    """Strip uninteresting metadata; produce a stable byte-stream.

    Steps (in order):
      1. Normalize line endings (CRLF → LF). Required on Windows.
      2. Split into per-file sections. A unified-diff per-file header
         starts at "diff --git ...".
      3. Drop `index <hex>..<hex> <mode>` lines — these embed worktree-
         local blob shas that vary by repo init order, defeating the key.
      4. Strip absolute-path leaks. git diff doesn't normally emit them
         but custom hunk drivers might.
      5. Sort per-file sections lexicographically. git already sorts
         files by path, but we're defensive.

    The output ends with a trailing newline so empty diffs hash to a
    deterministic non-empty value.
    """
    # 1. Normalize line endings.
    diff = diff.replace("\r\n", "\n").replace("\r", "\n")

    # 2. Split into per-file sections.
    parts: list = []
    current: list = []
    for line in diff.split("\n"):
        if line.startswith("diff --git "):
            if current:
                parts.append("\n".join(current))
                current = []
        current.append(line)
    if current:
        parts.append("\n".join(current))

    # 3/4. Strip volatile lines + absolute-path leaks.
    cleaned: list = []
    for part in parts:
        if not part.strip():
            continue
        lines = []
        for line in part.split("\n"):
            if line.startswith("index "):
                continue
            line = line.replace(worktree_abs_path, "")
            lines.append(line)
        cleaned.append("\n".join(lines))

    # 5. Sort per-file sections.
    cleaned.sort()

    return "\n".join(cleaned) + "\n"


def compute(worktree: str, base: Optional[str] = None) -> Tuple[str, str, int]:
    """Return ``(base_sha, diff_sha, canonical_bytes)`` for ``worktree``.

    ``base`` is the ref to diff against; defaults to the first commit
    reachable from HEAD when omitted (matches the spike's behavior).
    Raises ``FileNotFoundError`` if ``worktree`` is not a git checkout.
    """
    wt = os.path.abspath(worktree)
    if not os.path.isdir(os.path.join(wt, ".git")):
        raise FileNotFoundError(f"{wt} is not a git worktree")

    if base is None:
        base = _run(["git", "rev-list", "--max-parents=0", "HEAD"], wt).strip().split("\n")[0]
    base_full = _run(["git", "rev-parse", base], wt).strip()

    # --no-color/--no-ext-diff/--no-textconv/--no-renames + core.autocrlf=false
    # for determinism. Default -U3 context window; zero context would
    # also work but harms human-readability when --show-canonical is used.
    diff = _run(
        [
            "git",
            "-c",
            "core.autocrlf=false",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            base_full,
        ],
        wt,
    )

    canonical = canonicalize_diff(diff, wt)
    diff_sha = hashlib.sha256(canonical.encode("utf-8")).hexdigest()
    return base_full, diff_sha, len(canonical)


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Compute a stable content-hash for a cargo worktree's working state.",
    )
    ap.add_argument("worktree", help="Path to worktree")
    ap.add_argument(
        "--base",
        default=None,
        help="Base ref to diff against; default = first commit on HEAD",
    )
    ap.add_argument(
        "--show-canonical",
        action="store_true",
        help="Print the canonical diff to stderr for inspection",
    )
    args = ap.parse_args()

    try:
        base_full, diff_sha, n_bytes = compute(args.worktree, base=args.base)
    except FileNotFoundError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2

    if args.show_canonical:
        # Recompute the canonical body for display. Slightly redundant but
        # keeps `compute()` cheap for callers that don't need the body.
        wt = os.path.abspath(args.worktree)
        diff = _run(
            [
                "git",
                "-c",
                "core.autocrlf=false",
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                base_full,
            ],
            wt,
        )
        canonical = canonicalize_diff(diff, wt)
        sys.stderr.write("=== canonical diff ===\n")
        sys.stderr.write(canonical)
        sys.stderr.write("=== end ===\n")

    print(f"base_sha={base_full}")
    print(f"diff_sha={diff_sha}")
    print(f"canonical_bytes={n_bytes}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
