#!/usr/bin/env python3
"""
Fail the build on a NEW hand-rolled wall-clock helper in the runner's Rust.

WHAT THIS GATES
---------------
Plan `2026-10-04-runner-time-backoff-and-http-client-helpers-are-re-rolled-per-module`
(Phase 1).  The runner used to define `now_ms` / `now_millis` / `now_epoch_ms` /
`now_unix` / ... privately in ~40 modules, plus ~80 inline expressions.  The one
home is `src-tauri/src/util/time.rs` (`now_ms`, `now_ms_i64`, `now_secs`).

The check is keyed on the BODY, not the name -- a renamed copy
(`fn wall_clock_ms`) is exactly as bad as `fn now_ms` and a name regex cannot
see it:

    duration_since( ... UNIX_EPOCH

The match is made on whitespace-collapsed text, so a call split across lines
(`.duration_since(\\n    std::time::UNIX_EPOCH,\\n)`) is still found.

WHAT IS EXEMPT
--------------
  * `src-tauri/src/util/time.rs` itself (the one home);
  * test code: `#[cfg(test)]` items/modules, `tests.rs`, `*_tests.rs`,
    anything under a `tests/` directory;
  * the files listed in `scripts/helper-redefinition-allowlist.txt`.  That list
    holds the DEFERRED files (hot files whose conversion is sequenced behind
    their split plan, plan D5, plus files not yet converted).  It must only
    SHRINK, and it must be EXACT: a listed file with no remaining hit is itself
    a failure ("stale allowlist entry"), so a converted file cannot linger on it.

USAGE
-----
    python3 scripts/check_helper_redefinition.py            # check, exit 1 on failure
    python3 scripts/check_helper_redefinition.py --ci       # same (CI spelling)
    python3 scripts/check_helper_redefinition.py --write-allowlist
                                                    # regenerate the allowlist
                                                    # from the current tree
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "src-tauri" / "src"
HOME = "src-tauri/src/util/time.rs"
ALLOWLIST = ROOT / "scripts" / "helper-redefinition-allowlist.txt"

# `UNIX_EPOCH` must appear within this many chars of the `duration_since(` it
# belongs to, so the match cannot leap across unrelated statements.
MAX_GAP = 160


def blank_comments_and_strings(text: str) -> str:
    """Replace comment and string-literal content with spaces (newlines kept)."""
    out = []
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        two = text[i : i + 2]
        if two == "//":
            j = text.find("\n", i)
            j = n if j < 0 else j
            out.append(" " * (j - i))
            i = j
        elif two == "/*":
            depth, j = 1, i + 2
            while j < n and depth:
                if text[j : j + 2] == "/*":
                    depth += 1
                    j += 2
                elif text[j : j + 2] == "*/":
                    depth -= 1
                    j += 2
                else:
                    j += 1
            out.append(re.sub(r"[^\n]", " ", text[i:j]))
            i = j
        elif c == "r" and re.match(r'r#*"', text[i:]):
            m = re.match(r'r(#*)"', text[i:])
            hashes = m.group(1)
            end = text.find('"' + hashes, i + m.end())
            end = n if end < 0 else end + 1 + len(hashes)
            out.append(re.sub(r"[^\n]", " ", text[i:end]))
            i = end
        elif c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            j = min(j + 1, n)
            out.append(re.sub(r"[^\n]", " ", text[i:j]))
            i = j
        elif c == "'" and re.match(r"'(\\.[^']*|[^\\'])'", text[i:]):
            m = re.match(r"'(\\.[^']*|[^\\'])'", text[i:])
            out.append(" " * m.end())
            i += m.end()
        else:
            out.append(c)
            i += 1
    return "".join(out)


def blank_cfg_test(text: str) -> str:
    """Blank every item introduced by `#[cfg(test)]` (brace- or `;`-terminated)."""
    res = list(text)
    for m in re.finditer(r"#\[cfg\(test\)\]", text):
        j = m.end()
        n = len(text)
        depth = 0
        seen_brace = False
        while j < n:
            ch = text[j]
            if ch == "{":
                depth += 1
                seen_brace = True
            elif ch == "}":
                depth -= 1
                if seen_brace and depth == 0:
                    j += 1
                    break
            elif ch == ";" and not seen_brace and depth == 0:
                j += 1
                break
            j += 1
        for k in range(m.start(), min(j, n)):
            if res[k] != "\n":
                res[k] = " "
    return "".join(res)


def is_test_path(rel: str) -> bool:
    parts = rel.split("/")
    name = parts[-1]
    return (
        "tests" in parts[:-1]
        or name == "tests.rs"
        or name.endswith("_tests.rs")
        or name.endswith("_test.rs")
    )


def hits_in(path: Path) -> list[int]:
    text = path.read_text(encoding="utf-8", errors="replace")
    if "UNIX_EPOCH" not in text:
        return []
    text = blank_cfg_test(blank_comments_and_strings(text))
    lines = []
    for m in re.finditer(r"duration_since\s*\(", text):
        window = text[m.start() : m.start() + MAX_GAP]
        if "UNIX_EPOCH" in window:
            lines.append(text.count("\n", 0, m.start()) + 1)
    return lines


def scan() -> dict[str, list[int]]:
    found: dict[str, list[int]] = {}
    for p in sorted(SRC.rglob("*.rs")):
        rel = p.relative_to(ROOT).as_posix()
        if rel == HOME or is_test_path(rel):
            continue
        h = hits_in(p)
        if h:
            found[rel] = h
    return found


def read_allowlist() -> list[str]:
    if not ALLOWLIST.exists():
        return []
    return [
        l.strip()
        for l in ALLOWLIST.read_text().splitlines()
        if l.strip() and not l.lstrip().startswith("#")
    ]


HEADER = """\
# Files that still hand-roll a wall-clock read (`duration_since(..UNIX_EPOCH)`).
# Plan 2026-10-04-runner-time-backoff-and-http-client-helpers-are-re-rolled-per-module.
# This list must only SHRINK, and must be EXACT (scripts/check_helper_redefinition.py
# fails on a file listed here that no longer has a hit).  Hot files are converted
# by a follow-up PR per file after that file's split plan lands (plan D5).
"""


def main(argv: list[str]) -> int:
    found = scan()
    if "--write-allowlist" in argv:
        ALLOWLIST.write_text(HEADER + "\n".join(sorted(found)) + "\n")
        print(f"wrote {len(found)} entries to {ALLOWLIST.relative_to(ROOT)}")
        return 0
    allow = set(read_allowlist())
    bad = False
    for rel, lines in found.items():
        if rel not in allow:
            bad = True
            print(
                f"{rel}:{','.join(map(str, lines))}: hand-rolled wall-clock read; "
                f"use crate::util::time (now_ms / now_ms_i64 / now_secs)"
            )
    for rel in sorted(allow - set(found)):
        bad = True
        print(f"{rel}: stale allowlist entry (no remaining hit) -- delete it from "
              f"scripts/helper-redefinition-allowlist.txt")
    if bad:
        print("check_helper_redefinition: FAILED")
        return 1
    print(f"check_helper_redefinition: OK ({len(found)} allowlisted files remain)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
