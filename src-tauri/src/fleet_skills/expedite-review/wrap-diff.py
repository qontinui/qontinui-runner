#!/usr/bin/env python3
"""wrap-diff.py -- wrap a diff's long lines by CHARACTER for the reviewer.

Plan 2026-10-13-coord-expedited-land-lever-for-agents, Phase 6. The reviewer's
Read tool shows a bounded number of characters per line, and a requester
controls the diff, so one long line could hide added code past that column.
`fold` counts BYTES and splits multi-byte UTF-8 characters, which a requester
could use to make the reviewer's input undecodable. This wraps by code point.

Usage:
    python3 -I wrap-diff.py <diff-in> <review-out>

Writes <review-out> (every line at most WIDTH characters; a longer line
continues on the next, and every continuation line starts with MARK, which no
real diff line starts with -- otherwise the requester could choose the
character at column WIDTH+1 and make a continuation read as context, a removal
or a file header) and prints its line count. Bytes that are not UTF-8 (git
diffs a Latin-1 file verbatim) are shown as backslash-x escapes rather than refused:
a refusal would let one such byte in the requester's own diff opt it out of
review. Exit 2 only when the input cannot be read at all -- post nothing.
"""

from __future__ import annotations

import re
import sys

WIDTH = 1800
MARK = "\u21aa"  # RIGHTWARDS ARROW WITH HOOK


# Characters a reader may render as a line break or a terminal control: a lone
# CR, the other C0/C1 controls, U+2028/U+2029. Shown as visible escapes, so
# one diff line is one review line and nothing hides behind a control. Tab
# stays.
_INVISIBLE = re.compile("[\x00-\x08\x0a-\x1f\x7f-\x9f\u2028\u2029]")


def _visible(line: str) -> str:
    return _INVISIBLE.sub(
        lambda m: f"\\x{ord(m.group()):02x}" if ord(m.group()) < 0x100 else f"\\u{ord(m.group()):04x}", line
    )


def wrap_lines(text: str) -> list[str]:
    """Every line at most WIDTH characters; continuations start with MARK."""
    out: list[str] = []
    for line in text.split("\n"):
        line = _visible(line)
        if not line:
            out.append(line)
            continue
        out.append(line[:WIDTH])
        out.extend(MARK + line[i : i + WIDTH] for i in range(WIDTH, len(line), WIDTH))
    if out and out[-1] == "":
        out.pop()
    return out


def main(argv: list[str]) -> None:
    if len(argv) != 3:
        sys.exit("usage: wrap-diff.py <diff-in> <review-out>")
    try:
        with open(argv[1], encoding="utf-8", errors="backslashreplace", newline="") as f:
            text = f.read()
    except OSError as e:
        print(f"refused: the diff is not readable ({e.__class__.__name__}) -- post nothing", file=sys.stderr)
        sys.exit(2)
    out = wrap_lines(text)
    with open(argv[2], "w", encoding="utf-8", newline="") as f:
        f.write("\n".join(out) + "\n")
    print(len(out))


if __name__ == "__main__":
    main(sys.argv)
