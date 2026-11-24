#!/usr/bin/env python3
"""Census port of `extract_repos` (src-tauri/src/plan_workunit_adapter/body_push.rs).

A line-for-line Python copy of the Rust parse, kept in LOCKSTEP with it: any
change to `extract_repos` or its helpers must be mirrored here, and the
doc-comment numbers on `extract_repos` are this script's output. It reproduces
those numbers against the plan corpus without building the runner.

Run (read-only; never touches the dev-notes working tree or index):

    python3 extract_repos_census.py \
        --dev-notes <path to a qontinui-dev-notes clone> [--ref origin/main] \
        [--roster roster.txt] [--json out.json] [--verbose]

`--ref` defaults to `origin/main`; run `git -C <clone> fetch origin` first,
since the ref is read locally. `--roster` is one repo name per line, e.g.
`gh repo list qontinui --limit 200 --json name -q '.[].name' > roster.txt`;
without it no off-roster tokens are reported. `--json` writes
`{stem: [repos]}` for every file with a marker, so two runs (before/after a
parser change) can be diffed per file.

`--self-test` (alone) runs `extract_repos_cases.json`, the cases the Rust
test `the_shared_cases_hold_for_the_rust_parse` also runs, so the two ports
are checked against one set of inputs; ci.yml runs it on ubuntu.

With `--roster` it also lists files whose marker line names a roster repo
the parse did not emit (a partial drop, or a repo named as untouched).

Prints: the ref's sha, total `plans/*.md` files, files carrying a marker,
files yielding >= 1 repo, the marker-but-empty files, and every emitted token
not on the roster (with the files that emit it).
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from collections.abc import Iterator

# ---- the parse (mirror of body_push.rs) ------------------------------------

REPO_STOPWORDS = (
    "and", "plus", "the", "for", "only", "none", "n/a", "primary", "also", "with", "tbd",
    "various", "multiple", "unknown", "all", "any", "several", "same", "coord",
)
REPO_QUALIFIERS = ("primarily", "likely", "probably", "possibly", "mainly", "mostly")

REPO_DECL_MARKER = re.compile(r"\*\*Repo(?:\(s\)|s):\*\*")
REPO_DECL_PLAIN_LABEL = re.compile(r"^[A-Z][A-Za-z-]*:(?:\s|$)")
REPO_DECL_CUT = re.compile(r"\.(?:\s|$)|\s[—–](?:\s|$)|\s--(?:\s|$)|:(?:\s|$)")
REPO_DECL_LABEL = re.compile(r"\*\*[A-Z][A-Za-z-]*:")
REPO_DECL_BULLET = re.compile(r"^[-*+]\s+")
REPO_NAME = re.compile(r"^[a-z][a-z0-9_-]*[a-z0-9]$")
REPO_OWNER = re.compile(r"^[a-z0-9]+$")


def rust_lines(body: str) -> list[str]:
    """Rust `str::lines`: split on `\\n`, drop one trailing `\\r`, no final empty."""
    parts = body.split("\n")
    if parts and parts[-1] == "":
        parts.pop()
    # Only a line ended by `\n` sheds its `\r`; a bare final `\r` stays.
    ended = len(parts) if body.endswith("\n") else len(parts) - 1
    return [p[:-1] if i < ended and p.endswith("\r") else p for i, p in enumerate(parts)]


def marker_outside_code(line: str) -> str | None:
    for m in REPO_DECL_MARKER.finditer(line):
        if line[: m.start()].count("`") % 2 == 0:
            return line[m.end():]
    return None


def extract_repos(body: str) -> list[str]:
    lines = iter(rust_lines(body))
    tail = None
    quoted = False
    marker_item_indent = None
    for line in lines:
        t = marker_outside_code(line)
        if t is not None:
            quoted = line.lstrip().startswith(">")
            marker_item_indent = list_item_indent(line, quoted)
            tail = t
            break
    if tail is None:
        return []
    rest_lines = list(lines)
    items = bullet_items(iter(rest_lines), quoted, marker_item_indent) if not tail.strip() else []
    if items:
        declarations = items
    else:
        lines = iter(rest_lines)
        segments = [tail]
        if quoted:
            for line in lines:
                t = line.lstrip()
                if not t.startswith(">"):
                    break
                rest = t[1:]
                if "**" in rest or not rest.strip() or REPO_DECL_PLAIN_LABEL.match(rest.lstrip()):
                    break
                segments.append(rest)
        declarations = ["\n".join(segments)]
    out: list[str] = []
    for d in declarations:
        for repo in declaration_repos(d):
            if repo not in out:
                out.append(repo)
    return out


def list_item_indent(line: str, quoted: bool) -> int | None:
    if quoted:
        t = line.lstrip()
        if not t.startswith(">"):
            return None
        text = t[1:]
    else:
        text = line
    stripped = text.lstrip()
    return len(text) - len(stripped) if REPO_DECL_BULLET.match(stripped) else None


def bullet_items(lines: Iterator[str], quoted: bool, marker_item_indent: int | None) -> list[str]:
    items: list[str] = []
    item_indent = None
    for line in lines:
        if quoted:
            t = line.lstrip()
            if not t.startswith(">"):
                break
            text = t[1:]
        else:
            text = line
        stripped = text.lstrip()
        if not stripped:
            if item_indent is None:
                continue
            break
        indent = len(text) - len(stripped)
        if item_indent is not None and indent > item_indent:
            if items and items[-1].rstrip().endswith(",") and not REPO_DECL_BULLET.match(stripped):
                items[-1] += "\n" + stripped
            continue
        m = REPO_DECL_BULLET.match(stripped)
        if m is None or (item_indent is not None and indent < item_indent) \
                or (marker_item_indent is not None and indent <= marker_item_indent):
            break
        item = stripped[m.end():]
        lm = REPO_DECL_LABEL.search(item)
        if (lm is not None and lm.start() == 0) or REPO_DECL_PLAIN_LABEL.match(item):
            break
        item_indent = indent
        items.append(item)
    return items


def declaration_repos(text: str) -> list[str]:
    no_roles = strip_parenthetical(text)
    unlabelled = REPO_DECL_LABEL.split(no_roles, maxsplit=1)[0]
    return sentence_run_repos(unlabelled.replace("**", ""))


def sentence_run_repos(text: str) -> list[str]:
    out: list[str] = []
    rest: str | None = text
    while rest is not None:
        repos, rest = one_pair_repos(rest)
        for r in repos:
            if r not in out:
                out.append(r)
    return out


def one_pair_repos(text: str) -> tuple[list[str], str | None]:
    rest = text
    for attempt in range(2):  # at most one fallback sentence, opening with a name
        m = REPO_DECL_CUT.search(rest)
        sentence = rest[: m.start()] if m else rest
        if attempt > 0 and not opens_with_name(sentence):
            return [], None
        repos = sentence_repos(sentence)
        if repos:
            if m is None or m.group().startswith("."):
                return repos, None
            after = rest[m.end():]
            if says_untouched(after):
                untouched = last_chunk_repo(sentence)
                if untouched is not None:
                    repos = [r for r in repos if r != untouched]
            nxt = next_colon_pair(after) if m.group().startswith(":") else None
            return repos, nxt
        if m is None or not m.group().startswith(".") or declares_none(sentence):
            return [], None
        rest = rest[m.end():]
    return [], None


def last_chunk_repo(sentence: str) -> str | None:
    chunk = re.split(r"[,+;·→]", sentence)[-1]
    line = chunk.split("\n")[-1]
    for piece in (line, chunk):
        last = and_split(piece.split())[-1]
        repo = repo_from_chunk(last)
        if repo is not None:
            return repo
    return None


def next_colon_pair(after: str) -> str | None:
    if ";" not in after:
        return None
    role, nxt = after.split(";", 1)
    if any(m.group().startswith(".") for m in REPO_DECL_CUT.finditer(role)):
        return None
    m = REPO_DECL_CUT.search(nxt)
    return nxt if m is not None and m.group().startswith(":") else None


def declares_none(sentence: str) -> bool:
    if sentence.lstrip().lower().startswith("not applicable"):
        return True
    first = sentence.split()[:1]
    return bool(first) and _trim_token(first[0].replace("`", "")).lower() in ("none", "n/a", "tbd", "nothing")


def opens_with_name(sentence: str) -> bool:
    first = sentence.split()[:1]
    if not first:
        return False
    w = first[0]
    if any(c.isupper() for c in w):
        return False
    t = _trim_token(w.replace("`", ""))
    return "`" in w or "-" in t or "/" in t


UNTOUCHED = ("no change", "no changes", "not changed", "not modified", "not touched",
             "unchanged", "untouched")


def _ends_note(tail: str) -> bool:
    tail = tail.lstrip()
    for w in ("required", "needed", "expected", "planned"):
        if tail.startswith(w) and not (tail[len(w):] and tail[len(w)].isalnum()):
            tail = tail[len(w):]
            break
    tail = tail.lstrip()
    return not tail or tail[0] in ".,;)—–-"


def says_untouched(after: str) -> bool:
    lowered = after.lstrip()[:64].lower()
    for p in UNTOUCHED:
        if lowered.startswith(p):
            tail = lowered[len(p):]
            if not (tail and tail[0].isalnum()) and _ends_note(tail):
                return True
    return False


def sentence_repos(sentence: str) -> list[str]:
    out: list[str] = []
    for ai, arrow_part in enumerate(sentence.split("→")):
        for ci, chunk in enumerate(re.split(r"[,+;·]", arrow_part)):
            chunk_repos(chunk, ai > 0 and ci == 0, out)
    return out


def chunk_repos(chunk: str, after_arrow: bool, out: list[str]) -> None:
    # A line join is a chunk boundary, unless the phrase before it is
    # already several words: then the next line continues that phrase.
    phrases: list[list[str]] = []
    for i, piece in enumerate(chunk.split("\n")):
        words = piece.split()
        if i > 0 and len(and_split(phrases[-1])[-1]) >= 2 and not opens_with_name(piece.lstrip()):
            phrases[-1].extend(words)
        else:
            phrases.append(words)
    for pi, ph in enumerate(phrases):
        needs_name = after_arrow and pi == 0
        start = 0
        ends = [i for i in range(len(ph)) if ph[i].lower() in ("and", "plus", "then")] + [len(ph)]
        for end in ends:
            sub = ph[start:end]
            named = bool(sub) and opens_with_name(sub[0])
            if not (needs_name and not named):
                repo = repo_from_chunk(sub)
                if repo is not None and repo not in out:
                    out.append(repo)
            needs_name = end < len(ph) and ph[end].lower() != "and"
            start = end + 1


def and_split(words: list[str]) -> list[list[str]]:
    """Split on the joiners `and`, `plus`, `then` (Rust `is_joiner`)."""
    subs: list[list[str]] = [[]]
    for w in words:
        if w.lower() in ("and", "plus", "then"):
            subs.append([])
        else:
            subs[-1].append(w)
    return subs


def _trim_token(raw: str) -> str:
    keep = lambda c: c.isalnum() or c in "-_"  # noqa: E731
    start, end = 0, len(raw)
    while start < end and not keep(raw[start]):
        start += 1
    while end > start and not keep(raw[end - 1]):
        end -= 1
    return raw[start:end]


def repo_from_chunk(words: list[str]) -> str | None:
    tokens = []
    for w in words:
        raw = w.replace("`", "")
        trimmed = _trim_token(raw)
        if trimmed:
            tokens.append((raw, trimmed, "`" in w))
    if len(tokens) > 1 and tokens[0][1].lower() in REPO_QUALIFIERS:
        _, nxt, ticked = tokens[1]
        if not (ticked or "-" in nxt or "/" in nxt):
            return None
        tokens.pop(0)
    name_shaped = bool(tokens) and (tokens[0][2] or "-" in tokens[0][1] or "/" in tokens[0][1])
    for i in range(1, len(tokens)):
        t = tokens[i][1].lower()
        phase_tail = t == "for" or (t == "in" and i + 1 < len(tokens)
                                    and tokens[i + 1][1].lower().startswith("phase"))
        if t == "only" or phase_tail:
            bare_trailing_only = t == "only" and i + 1 == len(tokens)
            if not name_shaped and not bare_trailing_only:
                return None
            if t == "only" and i + 1 < len(tokens) and tokens[i + 1][1].lower() in ("if", "when", "unless"):
                return None
            tokens = tokens[:i]
            break
    if len(tokens) != 1:
        return None
    raw, token, _ = tokens[0]
    if raw.startswith(("~", ".", "/", "@")):
        return None
    lowered = token.lower()
    if lowered in REPO_STOPWORDS or lowered in REPO_QUALIFIERS:
        return None
    parts = lowered.split("/")
    if len(parts) == 1:
        name = parts[0]
    elif len(parts) == 2 and REPO_OWNER.match(parts[0]):
        name = parts[1]
    else:
        return None
    if len(name) > 64 or not REPO_NAME.match(name) or name in REPO_STOPWORDS or name in REPO_QUALIFIERS:
        return None
    return name


def opens_sentence(rest: str) -> bool:
    """Whitespace followed by an uppercase letter (Rust `opens_sentence`)."""
    stripped = rest.lstrip()
    return len(stripped) < len(rest) and stripped[:1].isupper()


def strip_parenthetical(s: str) -> str:
    out = []
    depth = 0
    last_inside = None
    for k, c in enumerate(s):
        if c == "(":
            if depth == 0:
                last_inside = None
            depth += 1
        elif c == ")":
            depth = max(0, depth - 1)
            if depth == 0:
                if last_inside == "." and opens_sentence(s[k + 1:]):
                    out.append(".")
                last_inside = None
        elif depth == 0:
            out.append(c)
        elif not c.isspace():
            last_inside = c
    return "".join(out)


def has_marker(body: str) -> bool:
    return any(marker_outside_code(line) is not None for line in rust_lines(body))


# ---- the census -------------------------------------------------------------


def git(dev_notes: str, *args: str) -> bytes:
    return subprocess.run(["git", "-C", dev_notes, *args], check=True, capture_output=True).stdout


def self_test() -> int:
    """Run the cases shared with the Rust test `the_shared_cases_hold_for_the_rust_parse`."""
    import os
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "extract_repos_cases.json")
    with open(path, encoding="utf-8") as f:
        cases = json.load(f)
    failed = 0
    for case in cases:
        got = extract_repos(case["body"])
        if got != case["repos"]:
            failed += 1
            print(f"MISMATCH {case['body']!r}: got {got}, want {case['repos']}")
    print(f"self-test: {len(cases) - failed}/{len(cases)} cases agree")
    return 1 if failed else 0


def main() -> int:
    if sys.argv[1:] == ["--self-test"]:
        return self_test()
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dev-notes", required=True)
    ap.add_argument("--ref", default="origin/main")
    ap.add_argument("--roster")
    ap.add_argument("--json")
    ap.add_argument("--verbose", action="store_true")
    a = ap.parse_args()

    sha = git(a.dev_notes, "rev-parse", a.ref).decode().strip()
    names = git(a.dev_notes, "ls-tree", "--name-only", sha, "plans/").decode().split("\n")
    paths = sorted(p for p in names if re.fullmatch(r"plans/[^/]+\.md", p))
    roster = None
    if a.roster:
        with open(a.roster, encoding="utf-8") as f:
            roster = {ln.strip() for ln in f if ln.strip()}

    results: dict[str, list[str]] = {}
    unemitted: dict[str, list[str]] = {}
    empty: list[str] = []
    off: dict[str, list[str]] = {}
    with_repo = 0
    for p in paths:
        body = git(a.dev_notes, "show", f"{sha}:{p}").decode("utf-8", errors="replace")
        if not has_marker(body):
            continue
        stem = p[len("plans/"):-len(".md")]
        repos = extract_repos(body)
        results[stem] = repos
        if repos:
            with_repo += 1
        else:
            empty.append(stem)
        if roster is not None:
            for r in repos:
                if r not in roster:
                    off.setdefault(r, []).append(stem)
            # A PARTIAL drop: a roster repo the marker line names that the
            # parse did not emit (some are named as untouched, so this is a
            # lead for review, not a defect count).
            tail = next(t for t in map(marker_outside_code, rust_lines(body)) if t is not None)
            missed = [n for n in sorted(roster) if n not in repos
                      and re.search(rf"(?<![\w/.-]){re.escape(n)}(?![\w/-])", tail)]
            if missed:
                unemitted[stem] = missed

    print(f"ref {a.ref} = {sha}")
    print(f"files {len(paths)}  marker {len(results)}  yield>=1 {with_repo}  marker-but-empty {len(empty)}")
    if roster is not None:
        print(f"off-roster tokens {len(off)} (roster {len(roster)} names)")
        print(f"named-but-not-emitted files {len(unemitted)} (roster repos on the marker line the parse dropped)")
        for stem in sorted(unemitted):
            print(f"  {stem}: {', '.join(unemitted[stem])}")
        for tok in sorted(off):
            print(f"  {tok}: {', '.join(off[tok][:5])}{' …' if len(off[tok]) > 5 else ''}")
    if a.verbose or len(empty) <= 60:
        print("marker-but-empty:")
        for s in empty:
            print(f"  {s}")
    if a.json:
        with open(a.json, "w", encoding="utf-8") as f:
            json.dump(results, f, indent=1, sort_keys=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
