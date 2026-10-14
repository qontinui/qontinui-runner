#!/usr/bin/env python3
"""parse-answer.py -- read the expedite-reviewer's answer, mechanically.

Plan 2026-10-13-coord-expedited-land-lever-for-agents, Phase 6. The reviewer
read a diff the requesting agent wrote, so its answer is diff-derived text.
The /expedite-review session does not interpret or retype it: it writes the
raw answer to a file and this script decides what it says.

Usage:
    python3 parse-answer.py <answer-file> <out-dir>

The answer must be exactly ONE non-empty line holding ONE JSON object with
exactly the keys `verdict` and `reason`: `verdict` is agree | dispute |
unreviewed, `reason` a string.

Exit 0  agree or dispute: writes <out-dir>/reason.txt (the reason, unchanged)
        and prints `verdict=agree` or `verdict=dispute` -- Step 4 posts that.
Exit 3  unreviewed: the reviewer could not read all of the input -- post
        nothing (a read failure is not evidence against the change).
Exit 2  not that shape -- the session may ask the same reviewer ONCE more.
"""

from __future__ import annotations

import json
import os
import sys
from typing import NoReturn

VERDICTS = ("agree", "dispute", "unreviewed")


def malformed(why: str) -> NoReturn:
    print(f"malformed answer: {why}", file=sys.stderr)
    sys.exit(2)


def main(argv: list[str]) -> None:
    if len(argv) != 3:
        malformed("usage: parse-answer.py <answer-file> <out-dir>")
    answer_file, out_dir = argv[1], argv[2]
    try:
        with open(answer_file, encoding="utf-8") as f:
            # "\n" only, for the same reason as parse-input.py.
            lines = [ln.rstrip("\r") for ln in f.read().split("\n") if ln.strip()]
    except (OSError, UnicodeDecodeError) as e:
        malformed(f"cannot read the answer file ({e.__class__.__name__})")
    # One code fence around the line (```json ... ```) is tolerated: models add
    # it, and it carries nothing. Anything else around the line is not.
    if len(lines) == 3 and lines[0].strip() in ("```", "```json") and lines[2].strip() == "```":
        lines = [lines[1]]
    if len(lines) != 1:
        malformed(f"{len(lines)} non-empty lines, not one")
    try:
        v = json.loads(lines[0])
    except json.JSONDecodeError:
        malformed("the line is not JSON")
    if not isinstance(v, dict) or set(v) != {"verdict", "reason"}:
        malformed('not one object with exactly the keys "verdict" and "reason"')
    verdict, reason = v["verdict"], v["reason"]
    if verdict not in VERDICTS:
        malformed("verdict is not agree, dispute or unreviewed")
    if not isinstance(reason, str):
        malformed("reason is not a string")
    # A lone surrogate (JSON allows "\\ud800") cannot be written as UTF-8:
    # REPLACE it rather than crash -- the reviewer's text is diff-derived.
    reason = reason.encode("utf-8", "replace").decode("utf-8")
    if verdict == "dispute" and not reason.strip():
        malformed("a dispute must say why: the reason is empty")
    if verdict == "unreviewed":
        print("verdict=unreviewed -- post nothing", file=sys.stderr)
        sys.exit(3)
    if not os.path.isdir(out_dir):
        malformed("the out-dir does not exist")
    with open(os.path.join(out_dir, "reason.txt"), "w", encoding="utf-8") as f:
        f.write(reason)
    print(f"verdict={verdict}")


if __name__ == "__main__":
    main(sys.argv)
