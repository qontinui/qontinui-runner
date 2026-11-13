#!/usr/bin/env python3
"""
Census, and ratchet, of the runner's PROVIDER-KEYED ("CLI-shaped") session code.

WHAT THIS MEASURES
------------------
The runner's session machinery says it is not a Claude client
(`src-tauri/src/session/provider_adapter.rs:9-10`) and then keys on the literal
Claude program almost everywhere: `"claude"` string compares, `CLAUDE_CONFIG_DIR`,
the `.claude/` config dir, a typed `/exit`, the two bypass-permission flag
spellings, the `--resume` / `--session-id` argv, `is_claude_*` detectors, and
`DEFAULT_PROVIDER` stamped onto records whatever was actually launched. Plan
`2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`
replaces those sites with a per-CLI profile manifest (`CliProfile`), a failure
taxonomy (`SessionFailure`) and a behaviour trait. This script is that plan's
Phase 1: it FINDS every such site, and a checked-in allowlist CLASSIFIES each one
into exactly one class:

    M  manifest fact            -- a `CliProfile` lookup replaces it (Phase 4/5)
    B  adapter behaviour        -- stays code, moves behind the behaviour trait
    F  failure classification   -- absorbed by the Phase 7 classifier
    S  sibling plan's state     -- screen-scraped / hook / statusline SESSION STATE,
                                   owned by plan
                                   `2026-09-20-terminal-session-state-comes-from-events-not-screen-scraping`
    K  legitimately Claude-only -- `claude_protocol/`, `claude_hook.rs`, the
                                   `auto_response.rs` scorer, the mock Claude CLI…

WHAT IS SCANNED
---------------
Fixed roots (the plan's census scope, not a discovered closure — this is a scope
census, not a safety gate):

  * non-test Rust under `src-tauri/src/{terminal,session,claude_session,
    process_capture,commands,install_effects_producer,bin}`;
  * non-test TS/TSX under `src/components/terminal`.

A root that does not exist FAILS the run: a missing root is UNKNOWN, never an
empty census.

"Non-test" means:

  * Rust: `#[cfg(test)]` / `#[cfg(all(.., test, ..))]` items (a `mod tests {}`,
    a fn, a `use`), and `#[test]` / `#[tokio::test]` / `#[rstest]` functions, are
    blanked before matching; a file whose first inner attribute is
    `#![cfg(test)]` is skipped. `#[cfg(any(test, ..))]` and `#[cfg(not(test))]`
    are compiled into a shipping build under some configuration, so they are
    SCANNED (fail closed). No out-of-line `#[cfg(test)] mod x;` exists under the
    roots today; if one appears its file is scanned (over-count, never under).
  * TS: `*.test.ts(x)`, `*.spec.ts(x)`, `*.testkit.ts(x)` (test-only helpers) and
    anything under a `__*__` directory (`__fixtures__`, `__golden__`,
    `__tests__`, `__mocks__`) are skipped.

Comments are blanked in both languages (a prose mention is not a site); string
literals are KEPT, because most of the patterns live inside them.

THE PATTERNS (pattern id -> what it matches)
--------------------------------------------
    lit:claude                 the string literal "claude" (' / " / ` quoted)
    lit:claude.exe             the string literal "claude.exe"
    lit:gemini                 the string literal "gemini"
    CLAUDE_CONFIG_DIR          the env var name
    .claude/                   the Claude config dir: `.claude/`, `".claude"`,
                               `.claude\\` — the plan names `.claude/`; the join
                               spelling `.join(".claude")` is the same fact and
                               is matched too
    /exit                      the typed graceful-exit command
    bypassPermissions          the permission-mode string
    --dangerously-skip-permissions
    --resume                   (not `--resume-foo`)
    --session-id               (not `--session-id-foo`)
    ident:is_claude            identifiers containing is_claude / claude_pids /
                               find_claude (and camelCase isClaude / claudePids /
                               findClaude)
    DEFAULT_PROVIDER

A HIT is one (file, line, pattern id). Two matches of the same pattern on one
line are one hit; two different patterns on one line are two hits.

THE ALLOWLIST AND WHY IT IS KEYED THE WAY IT IS
-----------------------------------------------
`scripts/cli-shaped-sites-allowlist.json`. Each entry is keyed by

    (file, item, pattern)   ->   {class | classes, count, reason}

where `item` is the ENCLOSING NAMED ITEM PATH of the hit — the chain of named
scopes around it (`mod`, `impl Type` / `<Type as Trait>`, `trait`, `fn`,
`struct`/`enum`, `const`/`static` in Rust; `function`, `class`, a named
`const`/`let` binding, a method, an object-literal property function in TS),
joined with `::`, or `<module>` / the module-level `const NAME` when there is no
enclosing scope.

Why not `file:line`: every unrelated edit above a site would break it, so the
allowlist would churn on every PR and be regenerated blind — at which point it
classifies nothing.

Why not `file + pattern + count` alone: that key cannot CLASSIFY. A file such as
`terminal/session.rs` holds manifest facts (`CLAUDE_CONFIG_DIR` injection),
adapter behaviour and failure paths side by side; one class per (file, pattern)
would force a wrong label onto most of them, and the census would then be the
seed sample again rather than a census. It also cannot see a site MOVING from a
classified function into a brand-new one within the same file.

`(file, item, pattern)` survives line drift and every edit that does not rename
or move the enclosing item, and it moves exactly when the thing that owns the
site changes — which is when the classification should be re-read. The accepted
churn cost: renaming a function with hits in it re-keys them (the ratchet names
both the vanished and the new key, so the fix is a rename in the JSON).

When one item holds hits of the same pattern in different classes, the entry
carries `classes` — one class per hit IN LINE ORDER within that item — instead of
`class`. That positional list is the one place a line-order assumption exists,
and it is confined to a single item.

THE RATCHET (`--ratchet`)
-------------------------
    a hit whose (file, item, pattern) has no entry          -> FAIL (new site)
    more hits for a key than its `count`                    -> FAIL (new site)
    fewer hits for a key than its `count`, or none          -> FAIL ("tighten it":
                                                               the census must
                                                               stay true, so a
                                                               removed site is
                                                               removed from the
                                                               allowlist in the
                                                               same diff)
    an entry with an empty `reason`, an unknown class, a
    `classes` list whose length != `count`, a duplicate key,
    or an unknown `format`                                  -> FAIL

so the classified population can only ever shrink, and every addition carries a
human-written class and reason in a reviewable diff.

`--update-allowlist` rewrites the file from the tree: counts that DROPPED keep
their class and reason (a strict improvement needs no fresh prose, but a
positional `classes` list is truncated from the end and should be re-read);
a key that GREW or is NEW gets an empty reason and no class, which `--ratchet`
then rejects until a human classifies it; vanished keys are removed.

WHAT IT DOES NOT SEE
--------------------
A syntactic census of a semantic property. Known holes, written down so a green
run is not over-read:

  * Provider keying spelled any other way: a `"claude"` built by `format!` or
    concatenation, a `Command::new(program)` whose `program` came from settings,
    `.starts_with("claude")`-style prefix matches on a longer literal
    (`"claude "`), Claude-only BEHAVIOUR with no literal in it at all (the
    stream-json lane's frame handling is only visible where it names a flag).
    The plan's census is of the named patterns; the Why section's sample was the
    seed, and this scan is the census of THOSE patterns, not of every
    Claude-shaped line.
  * Files outside the roots (e.g. `mcp/`, `ai_provider/`, `orchestration_loop/`,
    `account_migration.rs` callers outside `terminal/`). Widening the scope is a
    deliberate edit to `SCAN_ROOTS`.
  * TSX JSX text is lexed as code with a one-line-bounded string rule, so an
    apostrophe in JSX text cannot swallow more than the rest of its line.
  * Item labels are heuristic (header regexes over a comment/string-blanked
    skeleton). A mislabelled item is still a STABLE key — the ratchet's property
    is "the same code keys the same way run to run", which holds regardless.

USAGE
-----
    python3 scripts/check_cli_shaped_sites.py --ratchet          # CI gate
    python3 scripts/check_cli_shaped_sites.py --count            # total hits
    python3 scripts/check_cli_shaped_sites.py --count --class M  # hits of one class
    python3 scripts/check_cli_shaped_sites.py --count --class M --path-prefix src-tauri/src/terminal/
    python3 scripts/check_cli_shaped_sites.py --list             # every hit with its key
    python3 scripts/check_cli_shaped_sites.py --table            # markdown census rows
    python3 scripts/check_cli_shaped_sites.py --census --base-sha <sha>  # full census.md
    python3 scripts/check_cli_shaped_sites.py --update-allowlist
    python3 scripts/check_cli_shaped_sites.py --root <dir> ...   # a different tree

Pure stdlib (3.9+); no third-party imports.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import Counter, defaultdict
from dataclasses import dataclass
from pathlib import Path

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

#: The repo this script lives in.
DEFAULT_ROOT = Path(__file__).resolve().parent.parent

#: Scan roots, repo-relative POSIX. The plan's Phase 1 scope, verbatim.
RUST_ROOTS: tuple[str, ...] = (
    "src-tauri/src/terminal",
    "src-tauri/src/session",
    "src-tauri/src/claude_session",
    "src-tauri/src/process_capture",
    "src-tauri/src/commands",
    "src-tauri/src/install_effects_producer",
    "src-tauri/src/bin",
)
TS_ROOTS: tuple[str, ...] = ("src/components/terminal",)

#: Allowlist location, relative to the repo root.
ALLOWLIST_PATH = Path("scripts") / "cli-shaped-sites-allowlist.json"
ALLOWLIST_FORMAT = 1

CLASSES = ("M", "B", "F", "S", "K")
CLASS_NAMES = {
    "M": "manifest fact",
    "B": "adapter behaviour",
    "F": "failure classification",
    "S": "sibling plan's screen-scraped/hook state",
    "K": "legitimately Claude-only",
}

#: pattern id -> compiled regex, matched against comment-blanked source.
PATTERNS: dict[str, re.Pattern[str]] = {
    "lit:claude": re.compile(r"""(["'`])claude\1"""),
    "lit:claude.exe": re.compile(r"""(["'`])claude\.exe\1"""),
    "lit:gemini": re.compile(r"""(["'`])gemini\1"""),
    "CLAUDE_CONFIG_DIR": re.compile(r"\bCLAUDE_CONFIG_DIR\b"),
    ".claude/": re.compile(r"""(?<![\w.])\.claude(?=[/\\"'`])"""),
    "/exit": re.compile(r"/exit\b"),
    "bypassPermissions": re.compile(r"\bbypassPermissions\b"),
    "--dangerously-skip-permissions": re.compile(r"--dangerously-skip-permissions\b"),
    "--resume": re.compile(r"--resume(?![\w-])"),
    "--session-id": re.compile(r"--session-id(?![\w-])"),
    "ident:is_claude": re.compile(
        r"\b\w*(?:is_claude|claude_pids|find_claude|isClaude|claudePids|findClaude)\w*\b"
    ),
    "DEFAULT_PROVIDER": re.compile(r"\bDEFAULT_PROVIDER\b"),
}

TS_SKIP_FILE = re.compile(r"\.(?:test|spec|testkit)\.tsx?$")
TS_SKIP_DIR = re.compile(r"^__\w+__$")

# ---------------------------------------------------------------------------
# Lexing: two offset-preserving views of a file
#   code  -- comments blanked, strings kept         (patterns are matched here)
#   skel  -- comments AND string/regex bodies blanked (scopes are tracked here)
# ---------------------------------------------------------------------------


def _blank(chars: list[str], a: int, b: int) -> None:
    for i in range(a, b):
        if chars[i] != "\n":
            chars[i] = " "


def _is_ident(ch: str) -> bool:
    return ch.isalnum() or ch == "_"


def _raw_prefix_ok(src: str, i: int) -> bool:
    """`r` at i starts a raw string only if it is not the tail of an identifier (`br`/`cr` allowed)."""
    if i == 0 or not _is_ident(src[i - 1]):
        return True
    return src[i - 1] in "bc" and (i < 2 or not _is_ident(src[i - 2]))


def lex_rust(src: str) -> tuple[str, str]:
    code = list(src)
    skel = list(src)
    n = len(src)
    i = 0
    while i < n:
        c = src[i]
        if src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            _blank(code, i, j)
            _blank(skel, i, j)
            i = j
            continue
        if src.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if src.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif src.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            _blank(code, i, j)
            _blank(skel, i, j)
            i = j
            continue
        # raw strings: r"..", r#".."#, br#".."#
        if c == "r" and _raw_prefix_ok(src, i):
            m = re.match(r'r(#*)"', src[i : i + 260])
            if m:
                hashes = m.group(1)
                end = src.find('"' + hashes, i + len(m.group(0)))
                end = n if end < 0 else end + 1 + len(hashes)
                _blank(skel, i + len(m.group(0)), max(i + len(m.group(0)), end - 1 - len(hashes)))
                i = end
                continue
        if c == '"':
            j = i + 1
            while j < n and src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            _blank(skel, i + 1, min(j, n))
            i = j + 1
            continue
        if c == "'":
            # char literal vs lifetime
            if i + 1 < n and src[i + 1] == "\\":
                j = src.find("'", i + 2)
                if j > 0 and j - i <= 12:
                    _blank(skel, i + 1, j)
                    i = j + 1
                    continue
            elif i + 2 < n and src[i + 2] == "'":
                _blank(skel, i + 1, i + 2)
                i += 3
                continue
        i += 1
    return "".join(code), "".join(skel)


_TS_REGEX_PREV = set("(,=:[!&|?{};+-*%<>~^")
_TS_REGEX_KW = {"return", "typeof", "case", "in", "of", "delete", "void", "throw", "new", "yield", "await", "else", "do"}


def lex_ts(src: str) -> tuple[str, str]:
    code = list(src)
    skel = list(src)
    n = len(src)
    i = 0
    # mode stack: "code" frames carry a brace depth (for template interpolation)
    stack: list[list] = [["code", 0]]
    last_sig = ""  # last significant code char
    last_word = ""
    while i < n:
        top = stack[-1]
        c = src[i]
        if top[0] == "tmpl":
            if c == "\\":
                _blank(skel, i, min(i + 2, n))
                i += 2
                continue
            if c == "`":
                stack.pop()
                last_sig, last_word = "`", ""
                i += 1
                continue
            if src.startswith("${", i):
                stack.append(["code", 0])
                i += 2
                continue
            _blank(skel, i, i + 1)
            i += 1
            continue
        # code mode
        if src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            _blank(code, i, j)
            _blank(skel, i, j)
            i = j
            continue
        if src.startswith("/*", i):
            j = src.find("*/", i + 2)
            j = n if j < 0 else j + 2
            _blank(code, i, j)
            _blank(skel, i, j)
            i = j
            continue
        if c in "'\"":
            j = i + 1
            while j < n and src[j] != c and src[j] != "\n":
                j += 2 if src[j] == "\\" else 1
            _blank(skel, i + 1, min(j, n))
            i = j + 1
            last_sig, last_word = c, ""
            continue
        if c == "`":
            stack.append(["tmpl", 0])
            i += 1
            continue
        if c == "/" and (last_sig == "" or last_sig in _TS_REGEX_PREV or last_word in _TS_REGEX_KW):
            j, in_class = i + 1, False
            while j < n and src[j] != "\n":
                if src[j] == "\\":
                    j += 2
                    continue
                if src[j] == "[":
                    in_class = True
                elif src[j] == "]":
                    in_class = False
                elif src[j] == "/" and not in_class:
                    break
                j += 1
            if j < n and src[j] == "/":
                _blank(skel, i + 1, j)
                j += 1
                while j < n and src[j].isalpha():
                    j += 1
                i = j
                last_sig, last_word = ")", ""
                continue
        if c == "{":
            top[1] += 1
        elif c == "}":
            if len(stack) > 1 and top[1] == 0:
                stack.pop()  # end of ${ } interpolation
                i += 1
                continue
            top[1] -= 1
        if c.isalnum() or c in "_$":
            j = i
            while j < n and (src[j].isalnum() or src[j] in "_$"):
                j += 1
            last_word, last_sig = src[i:j], "a"
            i = j
            continue
        if not c.isspace():
            last_sig, last_word = c, ""
        i += 1
    return "".join(code), "".join(skel)


# ---------------------------------------------------------------------------
# Rust test-item stripping
# ---------------------------------------------------------------------------

_ATTR_RE = re.compile(r"#\[\s*(cfg\s*\((?P<cfg>.*?)\)|(?P<test>(?:tokio::|async_std::)?test\b[^\]]*|rstest\b[^\]]*))\s*\]", re.S)


def _cfg_is_test(pred: str) -> bool:
    pred = re.sub(r"\s+", "", pred)
    if pred == "test":
        return True
    if pred.startswith("all(") and re.search(r"(?<![\w(])test(?![\w])|\(test[,)]|,test[,)]", pred) and "not(test)" not in pred:
        return True
    return False


def _match_close(skel: str, open_idx: int, o: str = "{", c: str = "}") -> int:
    depth = 0
    for k in range(open_idx, len(skel)):
        if skel[k] == o:
            depth += 1
        elif skel[k] == c:
            depth -= 1
            if depth == 0:
                return k
    return len(skel) - 1


def strip_rust_tests(code: str, skel: str) -> tuple[str, str]:
    codel, skell = list(code), list(skel)
    for m in _ATTR_RE.finditer(skel):
        is_test = bool(m.group("test")) or (m.group("cfg") is not None and _cfg_is_test(m.group("cfg")))
        if not is_test:
            continue
        # find the item: first `{` or `;` at bracket depth 0 after the attribute
        j, pdepth = m.end(), 0
        while j < len(skel):
            ch = skel[j]
            if ch in "([":
                pdepth += 1
            elif ch in ")]":
                pdepth -= 1
            elif pdepth == 0 and ch == ";":
                break
            elif pdepth == 0 and ch == "{":
                j = _match_close(skel, j)
                break
            j += 1
        _blank(codel, m.start(), j + 1)
        _blank(skell, m.start(), j + 1)
    return "".join(codel), "".join(skell)


# ---------------------------------------------------------------------------
# Scope labelling
# ---------------------------------------------------------------------------

_RS_HEADERS = (
    re.compile(r"\bfn\s+(\w+)"),
    re.compile(r"\bmacro_rules!\s*(\w+)"),
    re.compile(r"\b(?:mod|trait|struct|enum|union)\s+(\w+)"),
    re.compile(r"\b(?:const|static)\s+(?:mut\s+)?(\w+)\s*:"),
)
_RS_IMPL = re.compile(r"\bimpl\b\s*(?:<.*?>)?\s*(.+?)\s*(?:\bwhere\b.*)?$", re.S)
_RS_STMT = re.compile(r"^\s*(?:#\[.*?\]\s*)*(?:pub(?:\([^)]*\))?\s+)?(?:const|static|type|struct|enum)\s+(?:mut\s+)?(\w+)", re.S)

_TS_KW = {"if", "for", "while", "switch", "catch", "function", "return", "else", "do", "try", "finally", "with"}
_TS_HEADERS = (
    re.compile(r"\bfunction\s*\*?\s*(\w+)"),
    re.compile(r"\bclass\s+(\w+)"),
    re.compile(r"\b(?:interface|enum|namespace)\s+(\w+)"),
    re.compile(r"\btype\s+(\w+)\s*(?:<[^=]*>)?\s*=\s*$"),
    re.compile(r"\b(?:const|let|var)\s+(\w+)\s*(?::[^=]*)?="),
    re.compile(r"(?:^|[\s,{])(\w+)\s*:\s*(?:async\s*)?(?:\([^()]*\)|\w+)\s*(?::[^=]*)?=>\s*$"),
    re.compile(r"(?:^|[\s;}])(?:(?:public|private|protected|static|async|get|set|readonly)\s+)*(\w+)\s*(?:<[^()]*>)?\s*\([^()]*\)\s*(?::\s*[^{}]*)?$"),
)
_SIG_RS = re.compile(r"\bfn\s+(\w+)")
_SIG_TS = re.compile(r"\bfunction\s*\*?\s*(\w+)")
_TS_STMT = re.compile(r"^\s*(?:export\s+)?(?:default\s+)?(?:declare\s+)?(?:const|let|var|function|class|type|interface|enum)\s+(\w+)", re.S)


def _impl_label(header: str) -> str | None:
    m = _RS_IMPL.search(header)
    if not m:
        return None
    body = re.sub(r"\s+", " ", m.group(1)).strip()
    parts = re.split(r"\s+for\s+", body, maxsplit=1)
    if len(parts) == 2:
        return f"<{parts[1].strip()} as {parts[0].strip()}>"
    return body


def _header_label(header: str, lang: str) -> str | None:
    if lang == "rs":
        if re.search(r"\bimpl\b", header) and not re.search(r"\bfn\s+\w+", header):
            return _impl_label(header)
        for rx in _RS_HEADERS:
            m = rx.search(header)
            if m:
                return m.group(1)
        return None
    for rx in _TS_HEADERS:
        m = rx.search(header)
        if m and m.group(1) not in _TS_KW:
            return m.group(1)
    return None


def item_paths(skel: str, lang: str) -> "ItemIndex":
    """Walk braces in the skeleton and record, for every offset, its named scope path."""
    stack: list[tuple[str | None, int]] = []  # (label, start of statement at this depth)
    events: list[tuple[int, tuple[str, ...]]] = []  # (offset, label tuple) change points
    stmt_start: list[int] = [0]
    last_closed: list[str | None] = [None]
    boundary = 0
    n = len(skel)

    def labels() -> tuple[str, ...]:
        return tuple(lbl for lbl, _ in stack if lbl)

    events.append((0, ()))
    for i in range(n):
        ch = skel[i]
        if ch == "{":
            header = skel[boundary:i]
            label = _header_label(header, lang)
            if label is None and lang == "ts" and skel[boundary - 1 : boundary] == "}" and last_closed[-1]:
                label = last_closed[-1]
            stack.append((label, stmt_start[-1]))
            stmt_start.append(i + 1)
            last_closed.append(None)
            events.append((i + 1, labels()))
            boundary = i + 1
        elif ch == "}":
            if stack:
                lbl, _ = stack.pop()
                stmt_start.pop()
                last_closed.pop()
                last_closed[-1] = lbl
            events.append((i + 1, labels()))
            boundary = i + 1
        elif ch == ";":
            stmt_start[-1] = i + 1
            last_closed[-1] = None
            boundary = i + 1
    return ItemIndex(events, skel, lang)


class ItemIndex:
    def __init__(self, events: list[tuple[int, tuple[str, ...]]], skel: str, lang: str):
        self.offsets = [e[0] for e in events]
        self.labels = [e[1] for e in events]
        self.skel = skel
        self.lang = lang

    def item_at(self, off: int) -> str:
        import bisect

        k = bisect.bisect_right(self.offsets, off) - 1
        path = list(self.labels[k])
        start = max(self.skel.rfind(";", 0, off), self.skel.rfind("}", 0, off), self.skel.rfind("{", 0, off)) + 1
        header = self.skel[start:off]
        # A hit inside a function SIGNATURE (before its body opens) belongs to
        # that function, not to its parent: `fn is_claude_image(..)` keys as
        # `is_claude_image`, the same as the hits in its body.
        ends = [p for p in (self.skel.find(ch, off) for ch in "{;}") if p >= 0]
        signature = self.skel[start : min(ends) if ends else len(self.skel)]
        sig = _SIG_RS.search(signature) if self.lang == "rs" else _SIG_TS.search(signature)
        if sig:
            return "::".join(path + [sig.group(1)])
        # Not inside any named scope: name the module-level statement.
        if not path:
            rx = _RS_STMT if self.lang == "rs" else _TS_STMT
            m = rx.match(header)
            return m.group(1) if m else "<module>"
        return "::".join(path)


# ---------------------------------------------------------------------------
# Scanning
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Hit:
    file: str
    line: int
    pattern: str
    item: str
    text: str

    @property
    def key(self) -> tuple[str, str, str]:
        return (self.file, self.item, self.pattern)


def iter_files(root: Path) -> list[tuple[Path, str]]:
    out: list[tuple[Path, str]] = []
    missing = [r for r in RUST_ROOTS + TS_ROOTS if not (root / r).is_dir()]
    if missing:
        raise SystemExit(f"FATAL: scan root(s) missing under {root}: {', '.join(missing)} — a missing root is UNKNOWN, not an empty census")
    for r in RUST_ROOTS:
        for p in sorted((root / r).rglob("*")):
            if p.is_file() and p.suffix.lower() == ".rs":
                out.append((p, "rs"))
    for r in TS_ROOTS:
        for p in sorted((root / r).rglob("*")):
            if not p.is_file() or p.suffix not in (".ts", ".tsx"):
                continue
            rel_parts = p.relative_to(root / r).parts
            if TS_SKIP_FILE.search(p.name) or any(TS_SKIP_DIR.match(d) for d in rel_parts[:-1]):
                continue
            out.append((p, "ts"))
    return out


def scan_file(path: Path, lang: str, rel: str) -> list[Hit]:
    src = path.read_text(encoding="utf-8", errors="replace")
    if lang == "rs":
        if re.match(r"\s*(?://[^\n]*\n\s*)*#!\[\s*cfg\s*\(\s*test\s*\)\s*\]", src):
            return []
        code, skel = lex_rust(src)
        code, skel = strip_rust_tests(code, skel)
    else:
        code, skel = lex_ts(src)
    index = item_paths(skel, lang)
    line_starts = [0] + [m.end() for m in re.finditer("\n", code)]
    lines = src.split("\n")
    import bisect

    seen: set[tuple[int, str]] = set()
    hits: list[Hit] = []
    for pid, rx in PATTERNS.items():
        for m in rx.finditer(code):
            ln = bisect.bisect_right(line_starts, m.start())
            if (ln, pid) in seen:
                continue
            seen.add((ln, pid))
            hits.append(Hit(rel, ln, pid, index.item_at(m.start()), lines[ln - 1].strip()))
    hits.sort(key=lambda h: (h.line, h.pattern))
    return hits


def scan(root: Path) -> list[Hit]:
    hits: list[Hit] = []
    for p, lang in iter_files(root):
        hits.extend(scan_file(p, lang, p.relative_to(root).as_posix()))
    hits.sort(key=lambda h: (h.file, h.line, h.pattern))
    return hits


# ---------------------------------------------------------------------------
# Allowlist
# ---------------------------------------------------------------------------


def load_allowlist(root: Path) -> tuple[list[dict], list[str]]:
    path = root / ALLOWLIST_PATH
    errors: list[str] = []
    if not path.is_file():
        return [], [f"allowlist {ALLOWLIST_PATH} is missing"]
    data = json.loads(path.read_text(encoding="utf-8"))
    if data.get("format") != ALLOWLIST_FORMAT:
        errors.append(f"allowlist format is {data.get('format')!r}; this checker reads format {ALLOWLIST_FORMAT}")
    entries = data.get("entries", [])
    seen: set[tuple[str, str, str]] = set()
    for e in entries:
        key = (e.get("file"), e.get("item"), e.get("pattern"))
        label = f"{key[0]} :: {key[1]} :: {key[2]}"
        if key in seen:
            errors.append(f"duplicate allowlist entry {label}")
        seen.add(key)
        if not isinstance(e.get("count"), int) or e["count"] < 1:
            errors.append(f"{label}: `count` must be a positive integer")
        if not str(e.get("reason", "")).strip():
            errors.append(f"{label}: empty `reason` — classify it and say why")
        if "classes" in e:
            cl = e["classes"]
            if not isinstance(cl, list) or len(cl) != e.get("count") or any(c not in CLASSES for c in cl):
                errors.append(f"{label}: `classes` must list exactly `count` classes from {'/'.join(CLASSES)}")
        elif e.get("class") not in CLASSES:
            errors.append(f"{label}: `class` must be one of {'/'.join(CLASSES)} (got {e.get('class')!r})")
    return entries, errors


def classify(hits: list[Hit], entries: list[dict]) -> list[tuple[Hit, dict | None, str | None]]:
    by_key = {(e["file"], e["item"], e["pattern"]): e for e in entries}
    ordinal: Counter = Counter()
    out = []
    for h in hits:
        e = by_key.get(h.key)
        cls = None
        if e is not None:
            k = ordinal[h.key]
            ordinal[h.key] += 1
            if "classes" in e:
                cl = e["classes"]
                cls = cl[k] if isinstance(cl, list) and k < len(cl) else None
            else:
                cls = e.get("class")
        out.append((h, e, cls))
    return out


def ratchet(hits: list[Hit], entries: list[dict], errors: list[str]) -> list[str]:
    problems = list(errors)
    counts = Counter(h.key for h in hits)
    first = {}
    for h in hits:
        first.setdefault(h.key, h)
    by_key = {(e.get("file"), e.get("item"), e.get("pattern")): e for e in entries}
    for key, n in sorted(counts.items()):
        e = by_key.get(key)
        h = first[key]
        if e is None:
            problems.append(f"NEW {h.file}:{h.line} [{h.pattern}] in `{h.item}` — {n} hit(s) with no classified allowlist entry: `{h.text}`")
        elif n > e.get("count", 0):
            problems.append(f"GREW {key[0]} :: {key[1]} :: {key[2]} — {n} hits, allowlist classifies {e.get('count')}")
        elif n < e.get("count", 0):
            problems.append(f"SHRANK {key[0]} :: {key[1]} :: {key[2]} — {n} hits, allowlist says {e.get('count')}: tighten it (--update-allowlist)")
    for key, e in sorted(by_key.items(), key=lambda kv: tuple(str(x) for x in kv[0])):
        if key not in counts:
            problems.append(f"GONE {key[0]} :: {key[1]} :: {key[2]} — allowlisted {e.get('count')}, now 0: remove it (--update-allowlist)")
    return problems


def update_allowlist(root: Path, hits: list[Hit], entries: list[dict]) -> int:
    counts = Counter(h.key for h in hits)
    by_key = {(e["file"], e["item"], e["pattern"]): e for e in entries}
    new_entries = []
    cleared = 0
    for key in sorted(counts):
        n = counts[key]
        old = by_key.get(key)
        e = {"file": key[0], "item": key[1], "pattern": key[2], "count": n}
        if old is not None and n <= old.get("count", 0):
            if "classes" in old:
                e["classes"] = old["classes"][:n]
            else:
                e["class"] = old.get("class")
            e["reason"] = old.get("reason", "")
            for k, v in old.items():  # carry unknown fields through
                e.setdefault(k, v)
        else:
            e["class"] = old.get("class") if old and "class" in old else None
            e["reason"] = ""
            cleared += 1
        new_entries.append(e)
    path = root / ALLOWLIST_PATH
    path.write_text(json.dumps({"format": ALLOWLIST_FORMAT, "entries": new_entries}, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"wrote {ALLOWLIST_PATH}: {len(new_entries)} entries, {cleared} need a class and reason", file=sys.stderr)
    return 0


# ---------------------------------------------------------------------------
# Output
# ---------------------------------------------------------------------------


def _md(s: str) -> str:
    return s.replace("|", "\\|").replace("\n", " ")


def table_rows(classified) -> list[str]:
    rows = []
    for h, e, cls in classified:
        reason = e.get("reason", "") if e else "UNCLASSIFIED"
        pat = h.pattern
        if pat == "ident:is_claude":
            tok = PATTERNS[pat].search(h.text)
            pat = f"ident:{tok.group(0)}" if tok else pat
        rows.append(f"| {cls or '?'} | `{h.file}:{h.line}` | `{_md(pat)}` | {_md(reason)} |")
    return rows


def census_doc(classified, base_sha: str | None) -> str:
    counts = Counter(cls or "?" for _, _, cls in classified)
    by_dir: dict[str, Counter] = defaultdict(Counter)
    for h, _, cls in classified:
        by_dir["/".join(h.file.split("/")[:3])][cls or "?"] += 1
    out = [
        "# Census: provider-keyed (CLI-shaped) session sites in qontinui-runner",
        "",
        "Phase 1 artifact of plan `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`.",
        "",
        f"- **Runner base:** qontinui-runner `{base_sha or 'UNKNOWN'}` (plus the Phase 1 script and allowlist)",
        "- **Generator:** `python3 scripts/check_cli_shaped_sites.py --census --base-sha "
        f"{base_sha or '<sha>'}` — every section down to and including **Rows** is emitted by the script; the classes and reasons come from `scripts/cli-shaped-sites-allowlist.json`. Any section after **Rows** is hand-written commentary.",
        f"- **Rows:** {len(classified)} — equal to `python3 scripts/check_cli_shaped_sites.py --count` by construction (one row per hit; a hit is one (file, line, pattern)).",
        "- **Scope:** non-test Rust under `src-tauri/src/{terminal,session,claude_session,process_capture,commands,install_effects_producer,bin}`; non-test TS/TSX under `src/components/terminal`. Comments are not hits; string literals are.",
        "",
        "## Classes",
        "",
        "| Class | Meaning | Hits |",
        "|---|---|---|",
    ]
    for c in CLASSES:
        out.append(f"| **{c}** | {CLASS_NAMES[c]} | {counts.get(c, 0)} |")
    if counts.get("?"):
        out.append(f"| **?** | UNCLASSIFIED (ratchet is red) | {counts['?']} |")
    out.append(f"| | **total** | **{len(classified)}** |")
    out += ["", "## By directory", "", "| Directory | " + " | ".join(CLASSES) + " | total |", "|---|" + "---|" * (len(CLASSES) + 1)]
    for d in sorted(by_dir):
        c = by_dir[d]
        out.append(f"| `{d}` | " + " | ".join(str(c.get(k, 0)) for k in CLASSES) + f" | {sum(c.values())} |")
    m_total = counts.get("M", 0)
    out += ["", "## Scope rule (Phase 1)", ""]
    if m_total > 60:
        m_by_dir = sorted(((d, c.get("M", 0)) for d, c in by_dir.items() if c.get("M")), key=lambda x: (not x[0].startswith("src-tauri/src/terminal"), x[0]))
        out.append(f"Class **M** has **{m_total}** sites, above the plan's ~60 threshold, so **Phase 5 splits by directory, `terminal/` first**; the remainder is a tracked follow-up and the ratchet holds the line. Nothing is dropped from scope. M split, in the order Phase 5 works it:")
        out.append("")
        for d, k in m_by_dir:
            out.append(f"- `{d}` — {k}")
    else:
        out.append(f"Class **M** has **{m_total}** sites, within the plan's ~60 threshold: Phase 5 is not split.")
    out += ["", "## Rows", "", "| Class | Site | Pattern | Reason |", "|---|---|---|---|"]
    out += table_rows(classified)
    return "\n".join(out) + "\n"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0] if __doc__ else None)
    ap.add_argument("--root", type=Path, default=DEFAULT_ROOT, help="repo root to scan (default: this script's repo)")
    mode = ap.add_mutually_exclusive_group()
    mode.add_argument("--ratchet", action="store_true", help="fail when a hit is not covered by the classified allowlist (CI gate)")
    mode.add_argument("--count", action="store_true", help="print the number of hits (filtered by --class / --path-prefix)")
    mode.add_argument("--list", action="store_true", help="print every hit with its allowlist key")
    mode.add_argument("--table", action="store_true", help="print the census rows as a markdown table")
    mode.add_argument("--census", action="store_true", help="print the full census.md document")
    mode.add_argument("--update-allowlist", action="store_true", help="rewrite the allowlist from the tree (new/grown keys get an empty reason)")
    ap.add_argument("--class", dest="cls", choices=CLASSES, help="with --count/--list/--table: only hits of this class")
    ap.add_argument("--path-prefix", default="", help="with --count/--list/--table: only hits in files under this repo-relative prefix")
    ap.add_argument("--base-sha", default=None, help="with --census: the runner sha to stamp in the header")
    args = ap.parse_args(argv)

    root = args.root.resolve()
    hits = scan(root)
    entries, errors = load_allowlist(root)

    if args.update_allowlist:
        return update_allowlist(root, hits, entries)

    classified = classify(hits, entries)
    if args.path_prefix:
        classified = [t for t in classified if t[0].file.startswith(args.path_prefix)]
    if args.cls:
        unclassified = sum(1 for _, _, c in classified if c is None)
        if unclassified:
            print(f"WARNING: {unclassified} hit(s) have no class and are not counted under --class {args.cls}", file=sys.stderr)
        classified = [t for t in classified if t[2] == args.cls]

    if args.count:
        print(len(classified))
        return 0
    if args.list:
        for h, _, cls in classified:
            print(f"{cls or '?'}\t{h.file}:{h.line}\t{h.pattern}\t{h.item}\t{h.text}")
        return 0
    if args.table:
        print("| Class | Site | Pattern | Reason |\n|---|---|---|---|")
        print("\n".join(table_rows(classified)))
        return 0
    if args.census:
        sys.stdout.write(census_doc(classified, args.base_sha))
        return 0

    # default and --ratchet: the gate
    problems = ratchet(hits, entries, errors)
    if problems:
        print(f"FAIL: {len(problems)} CLI-shaped-site ratchet problem(s):", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        print(
            "\nA provider-keyed site (\"claude\", CLAUDE_CONFIG_DIR, .claude/, /exit, bypass flags, --resume/--session-id, "
            "is_claude*, DEFAULT_PROVIDER) must be classified M/B/F/S/K with a reason in "
            f"{ALLOWLIST_PATH.as_posix()} — plan 2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy. "
            "Prefer a CliProfile lookup over a new literal.",
            file=sys.stderr,
        )
        return 1
    print(f"OK: {len(hits)} CLI-shaped site(s), all classified ({len(entries)} allowlist entries)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
