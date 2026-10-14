#!/usr/bin/env python3
"""parse-input.py -- validate the /expedite-review input line, mechanically.

Plan 2026-10-13-coord-expedited-land-lever-for-agents, Phase 6. The review
gate's continuation prompt carries one JSON line between the markers
-----BEGIN EXPEDITE REVIEW INPUT----- / -----END EXPEDITE REVIEW INPUT-----.
Part of it (`independence`, `disclosure`) is free text the requesting agent
wrote, and the rest reaches a command line in the skill's later steps. So the
shape checks are a TESTED script, not the model's judgement, and the claim is
written to disk by this script (by value: parsed and re-serialized).

Usage:
    python3 parse-input.py <request_id> <input-file> <out-dir>

<input-file> holds the one JSON line (the session writes it with its Write
tool). On success this writes <out-dir>/claim.json -- exactly the `reason`,
`independence` and `disclosure` fields, pretty-printed, with any line over 1800
characters wrapped onto continuation lines that start with U+21AA -- and prints
three lines for the fetch step:
    repo=<owner/name>
    base_sha=<40 hex>
    head_sha=<40 hex>

Exit 0 on success. Exit 2 when ANY rule fails, with the rule on stderr: the
skill then posts nothing, and the item counts `unreviewed`.
"""

from __future__ import annotations

import json
import os
import re
import sys
from typing import NoReturn

UUID_RE = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
# Each side of owner/name: starts with an alphanumeric, so `.`, `..` and a
# leading `-` (an option, to some tools) can never pass as a path component.
REPO_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*/[A-Za-z0-9][A-Za-z0-9._-]*$")
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
REASONS = ("red_main_remedy", "coord_stall")
# The same wrap as wrap-diff.py, for claim.json.
WIDTH = 1800
MARK = "\u21aa"  # RIGHTWARDS ARROW WITH HOOK


def refuse(why: str) -> NoReturn:
    print(f"refused: {why} -- post nothing", file=sys.stderr)
    sys.exit(2)


def main(argv: list[str]) -> None:
    if len(argv) != 4:
        refuse("usage: parse-input.py <request_id> <input-file> <out-dir>")
    request_id, input_file, out_dir = argv[1], argv[2], argv[3]
    if not UUID_RE.fullmatch(request_id):
        refuse("the skill argument is not a lower-case uuid")
    try:
        with open(input_file, encoding="utf-8") as f:
            # Split on "\n" ONLY: str.splitlines() also splits on U+2028,
            # U+2029 and U+0085, which serde_json does not escape -- so a
            # requester could break its own line and switch off the review.
            lines = [ln.rstrip("\r") for ln in f.read().split("\n") if ln.strip()]
    except (OSError, UnicodeDecodeError) as e:
        refuse(f"cannot read the input file ({e.__class__.__name__})")
    if len(lines) != 1:
        refuse(f"the input is {len(lines)} non-empty lines, not one")
    try:
        v = json.loads(lines[0])
    except json.JSONDecodeError:
        refuse("the input line is not JSON")
    if not isinstance(v, dict):
        refuse("the input line is not one JSON object")
    if v.get("request_id") != request_id:
        refuse("the input's request_id differs from the skill argument")
    repo, base, head = v.get("repo"), v.get("base_sha"), v.get("head_sha")
    if not isinstance(repo, str) or not REPO_RE.fullmatch(repo):
        refuse("repo is not an owner/name slug")
    if base is None:
        refuse("base_sha is null: there is no base to pin the diff against")
    if not isinstance(base, str) or not SHA_RE.fullmatch(base):
        refuse("base_sha is not 40 lower-case hex")
    if not isinstance(head, str) or not SHA_RE.fullmatch(head):
        refuse("head_sha is not 40 lower-case hex")
    if v.get("reason") not in REASONS:
        refuse("reason is not red_main_remedy or coord_stall")
    independence = v.get("independence")
    if not isinstance(independence, dict) or not all(
        isinstance(independence.get(k), str) for k in ("claim", "verified", "against")
    ):
        refuse("independence is not an object of claim/verified/against strings")
    disclosure = v.get("disclosure")
    if not isinstance(disclosure, str):
        refuse("disclosure is not a string")
    if not os.path.isdir(out_dir):
        refuse("the out-dir does not exist")
    claim = {"reason": v["reason"], "independence": independence, "disclosure": disclosure}
    # Wrapped exactly like the diff (wrap-diff.py): one JSON string can be one
    # very long line, and a reader that cannot see a whole line answers
    # `unreviewed` -- so a long disclosure would otherwise opt out of review.
    text = json.dumps(claim, ensure_ascii=False, indent=2)
    # U+2028/U+2029/U+0085 stay raw under ensure_ascii=False; a reader may break
    # a line on them. Escape them back (JSON reads them the same).
    for ch in ("\u2028", "\u2029", "\u0085"):
        text = text.replace(ch, "\\u%04x" % ord(ch))
    lines: list[str] = []
    for line in text.split("\n"):
        lines.append(line[:WIDTH])
        lines.extend(MARK + line[i : i + WIDTH] for i in range(WIDTH, len(line), WIDTH))
    with open(os.path.join(out_dir, "claim.json"), "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")
    print(f"repo={repo}")
    print(f"base_sha={base}")
    print(f"head_sha={head}")


if __name__ == "__main__":
    main(sys.argv)
