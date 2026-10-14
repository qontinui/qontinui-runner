#!/usr/bin/env python3
"""
Fail the build on a NEW reqwest client constructed outside the sanctioned factory.

WHAT THIS GATES
---------------
Constructing a `reqwest` client anywhere but `src-tauri/src/util/http.rs`:

    reqwest::Client::new()                    reqwest::Client::builder()
    reqwest::blocking::Client::new()          reqwest::blocking::Client::builder()
    reqwest::ClientBuilder::new()             reqwest::blocking::ClientBuilder::new()
    reqwest::Client::default()                reqwest::ClientBuilder::default()
    reqwest::blocking::Client::default()      reqwest::blocking::ClientBuilder::default()
    Client::new() / Client::builder() / Client::default()
                                              -- where `Client` is imported from reqwest
                                                 (including `use reqwest::Client as Http;`
                                                 and `use reqwest::blocking;` + `blocking::Client::new()`)
    rq::Client::new()                         -- where `use reqwest as rq;` renames the crate

`default` is in that list because reqwest implements `Default` for all four
client types, so `Client::default()` is a compiling, unconverted, GaiResolver-
backed construction. It used to walk straight through this check.

KNOWN OUT OF REACH
------------------
The TYPED spelling `let c: reqwest::Client = Default::default();` (and
`let c: reqwest::Client = <_>::default();`) names no reqwest path at the call
site: resolving it needs type inference, which a regex scanner does not have.
It is documented here rather than papered over with a pattern that would only
appear to cover it. The `// reqwest-client-ok:` escape and code review are the
controls for that form.

The sanctioned constructors are, in `src-tauri/src/util/http.rs`:

    util::http::client_builder()          -> reqwest::ClientBuilder
    util::http::blocking_client_builder() -> reqwest::blocking::ClientBuilder
    util::http::ResettableClient::new(name, factory)   (long-lived clients)

WHY IT EXISTS
-------------
A `reqwest::Client` owns its DNS resolver. With reqwest's `hickory-dns`
feature (runner #918) each client snapshots the adapter DNS servers ONCE, on
first lookup, and never rebuilds; after a WLAN blip on 2026-09-16 the runner's
process-lifetime coord clients failed for 90+ minutes while curl, git and
freshly-built clients recovered in one. Without that feature, reqwest's default
`GaiResolver` spends a tokio BLOCKING-POOL thread per lookup, which is the
flood #918 fixed. The factory resolves through `util::dns::shared()` — the OS
resolver on a dedicated bounded thread lane — which has neither defect. A client
built any other way silently gets one of the two, and nothing at runtime says so,
so the class gets a machine check.

Plan: 2026-09-16-runner-long-lived-http-clients-freeze-dns-state-across-network-changes

WHAT IS SCANNED
---------------
`src-tauri/src/**/*.rs` (suffix matched case-insensitively) — the bin and lib
crates, the only code that can reach `util::http`. Sibling crates under
`crates/` cannot depend on the runner crate, so a construction there has no
factory to route through; they are out of scope and `--list-roots` says so.

Before matching, comments and string/char literals are blanked (a `Client::new()`
in a doc comment is not a construction) and every item attributed
`#[cfg(test)]` is removed — test code builds clients against local fixtures and
never ships.

ESCAPE HATCH
------------
A construction that genuinely must bypass the factory (e.g. a test of the
resolver itself outside a `#[cfg(test)]` item) carries a comment on the SAME
line or the line IMMEDIATELY ABOVE:

    // reqwest-client-ok: <reason>

An empty reason does not count.

THE BASELINE (a ratchet)
------------------------
`scripts/reqwest-client-construction-baseline.json` records, per file, how many
unconverted constructions are accepted today. The check FAILS when a file has
MORE constructions than its entry (a new site) and ALSO when it has FEWER (a site
was converted: tighten the entry, so the baseline can only shrink). A file absent
from the baseline allows zero. Wave B of the plan converted every site and
emptied the baseline; it is kept (empty) so the file format stays checked, and
must not grow — use the escape comment for a genuine exception.

A per-file COUNT cannot see a swap (one site converted and a new one added in
the same file in the same change). With the baseline empty that hole is closed:
every construction outside the factory fails outright.

USAGE
-----
    python3 scripts/check_reqwest_client_construction.py            # the check
    python3 scripts/check_reqwest_client_construction.py --ci       # ::error:: annotations
    python3 scripts/check_reqwest_client_construction.py --list     # every site found
    python3 scripts/check_reqwest_client_construction.py --list-roots
    python3 scripts/check_reqwest_client_construction.py --update-baseline
    python3 scripts/check_reqwest_client_construction.py --self-test

Pure stdlib, Python 3.9+.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

DEFAULT_ROOT = Path(__file__).resolve().parent.parent
SCAN_ROOT = Path("src-tauri/src")
BASELINE_PATH = Path("scripts/reqwest-client-construction-baseline.json")
BASELINE_FORMAT = 1

# Repo-relative POSIX paths allowed to construct clients directly.
FACTORY_FILES = frozenset(
    {
        "src-tauri/src/util/http.rs",
        "src-tauri/src/util/dns.rs",
    }
)

ESCAPE_RE = re.compile(r"//\s*reqwest-client-ok:\s*\S")

# `default` is as much a construction as `new`: reqwest implements Default for
# Client, ClientBuilder and both blocking twins.
_CLIENT_CTORS = r"(?:new|builder|default)"
_BUILDER_CTORS = r"(?:new|default)"
_CTOR_TAIL = (
    rf"(?:blocking\s*::\s*)?"
    rf"(?:Client\s*::\s*{_CLIENT_CTORS}|ClientBuilder\s*::\s*{_BUILDER_CTORS})\s*\("
)

_QUAL_RE = re.compile(r"(?<![\w:])reqwest\s*::\s*" + _CTOR_TAIL)


@dataclass(frozen=True)
class Finding:
    path: str
    line: int
    snippet: str


# ---------------------------------------------------------------------------
# Lexing: blank comments and literals, keep offsets and newlines
# ---------------------------------------------------------------------------


def strip_noise(src: str) -> str:
    """Replace comments and string/char literal CONTENTS with spaces.

    Newlines are preserved so offsets map to the original line numbers.
    """
    out = list(src)
    n = len(src)
    i = 0

    def blank(a: int, b: int) -> None:
        for k in range(a, min(b, n)):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        c = src[i]
        nxt = src[i + 1] if i + 1 < n else ""
        if c == "/" and nxt == "/":
            j = src.find("\n", i)
            j = n if j == -1 else j
            blank(i, j)
            i = j
        elif c == "/" and nxt == "*":
            depth = 1
            j = i + 2
            while j < n and depth:
                if src.startswith("/*", j):
                    depth += 1
                    j += 2
                elif src.startswith("*/", j):
                    depth -= 1
                    j += 2
                else:
                    j += 1
            blank(i, j)
            i = j
        elif c == "r" and re.match(r'r#*"', src[i:]) and not (i and (src[i - 1].isalnum() or src[i - 1] == "_")):
            m = re.match(r'r(#*)"', src[i:])
            hashes = m.group(1)
            start = i + len(m.group(0))
            end = src.find('"' + hashes, start)
            end = n if end == -1 else end + 1 + len(hashes)
            blank(start, end - 1 - len(hashes))
            i = end
        elif c == '"':
            j = i + 1
            while j < n and src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            blank(i + 1, j)
            i = j + 1
        elif c == "'":
            m = re.match(r"'(\\(?:u\{[0-9a-fA-F]+\}|x[0-9a-fA-F]{2}|.)|[^\\'\n])'", src[i:])
            if m:
                blank(i + 1, i + len(m.group(0)) - 1)
                i += len(m.group(0))
            else:
                i += 1  # a lifetime
        else:
            i += 1
    return "".join(out)


def _match_brace(src: str, open_idx: int) -> int:
    depth = 0
    for k in range(open_idx, len(src)):
        if src[k] == "{":
            depth += 1
        elif src[k] == "}":
            depth -= 1
            if depth == 0:
                return k
    return len(src) - 1


_CFG_ATTR_RE = re.compile(r"#\s*\[\s*cfg\s*\(")


def _match_paren(src: str, open_idx: int) -> int:
    """Index of the `)` closing the `(` at `open_idx`, or the last index."""
    depth = 0
    for k in range(open_idx, len(src)):
        if src[k] == "(":
            depth += 1
        elif src[k] == ")":
            depth -= 1
            if depth == 0:
                return k
    return len(src) - 1


def cfg_selects_test(pred: str) -> bool:
    """Does this `cfg(...)` predicate select TEST builds?

    `#[cfg(test)]` is the common spelling but not the only one: a helper
    attributed `#[cfg(all(test, feature = "x"))]` or `#[cfg(any(test, doc))]` is
    test code too, and treating it as shipping code produced false positives.
    `not(test)` is the opposite claim and must NOT match, or real shipping code
    would be skipped silently -- a false NEGATIVE, which is worse.
    """
    i = 0
    n = len(pred)
    stack: list[bool] = []  # one entry per open paren: did it open a not(…)?
    negated = 0
    while i < n:
        m = re.match(r"(\w+)\s*\(", pred[i:])
        if m:
            is_not = m.group(1) == "not"
            stack.append(is_not)
            negated += is_not
            i += len(m.group(0))
            continue
        if pred[i] == ")":
            if stack:
                negated -= stack.pop()
            i += 1
            continue
        m = re.match(r"\w+", pred[i:])
        if m:
            if m.group(0) == "test" and negated == 0:
                return True
            i += len(m.group(0))
            continue
        i += 1
    return False


def _item_end(src: str, start: int) -> int:
    """End index of the item beginning at `start`.

    Its body's closing brace, or the `;` of a body-less item (`mod tests;`,
    `use …;`) -- but only a brace or semicolon at depth 0 of `()` and `[]`.
    Scanning for a bare `;` cut `const N: [u8; 4] = …;` off inside its own type
    and left the rest of the item unblanked.
    """
    depth = 0
    for k in range(start, len(src)):
        c = src[k]
        if c in "([":
            depth += 1
        elif c in ")]":
            depth -= 1
        elif depth <= 0:
            if c == "{":
                return _match_brace(src, k)
            if c == ";":
                return k
    return -1


def strip_test_items(src: str) -> str:
    """Blank every item attributed a test-selecting `cfg` (on noise-stripped text)."""
    out = list(src)
    for m in _CFG_ATTR_RE.finditer(src):
        open_paren = src.index("(", m.start())
        close_paren = _match_paren(src, open_paren)
        if not cfg_selects_test(src[open_paren + 1 : close_paren]):
            continue
        # Past the attribute's own `]`, so its brackets cannot be mistaken for
        # the item's.
        bracket = src.find("]", close_paren)
        if bracket == -1:
            continue
        end = _item_end(src, bracket + 1)
        if end == -1:
            continue
        for k in range(m.start(), end + 1):
            if out[k] != "\n":
                out[k] = " "
    return "".join(out)


# ---------------------------------------------------------------------------
# Import resolution: which bare names mean a reqwest client type
# ---------------------------------------------------------------------------

_USE_RE = re.compile(r"\buse\s+(?:::)?reqwest\s*::\s*(.*?);", re.S)

# `use reqwest as rq;` renames the CRATE, so `rq::Client::new()` is a fully
# qualified construction that `_QUAL_RE` (which looks for the literal
# `reqwest`) cannot see.
_USE_CRATE_ALIAS_RE = re.compile(r"\buse\s+(?:::)?reqwest\s+as\s+(\w+)\s*;")


def _flatten_use_tree(tree: str, prefix: list[str]) -> list[tuple[list[str], str]]:
    """Expand `a::{b, c::{d as e}}` into (path, bound_name) pairs."""
    tree = tree.strip()
    results: list[tuple[list[str], str]] = []
    if not tree:
        return results
    brace = tree.find("{")
    if brace != -1 and tree.endswith("}"):
        head = [p.strip() for p in tree[:brace].split("::") if p.strip()]
        inner = tree[brace + 1 : -1]
        depth = 0
        cur = ""
        parts: list[str] = []
        for ch in inner:
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
            if ch == "," and depth == 0:
                parts.append(cur)
                cur = ""
            else:
                cur += ch
        parts.append(cur)
        for p in parts:
            results += _flatten_use_tree(p, prefix + head)
        return results
    alias = None
    m = re.match(r"(.*?)\s+as\s+(\w+)$", tree)
    if m:
        tree, alias = m.group(1), m.group(2)
    path = prefix + [p.strip() for p in tree.split("::") if p.strip()]
    if not path:
        return results
    name = alias or path[-1]
    if path[-1] == "self":
        path = path[:-1]
        name = alias or (path[-1] if path else "reqwest")
    results.append((path, name))
    return results


def construction_patterns(src: str) -> list[re.Pattern]:
    """Patterns for bare spellings made reachable by this file's reqwest imports."""
    pats: list[re.Pattern] = []
    for m in _USE_CRATE_ALIAS_RE.finditer(src):
        alias = re.escape(m.group(1))
        pats.append(re.compile(r"(?<![\w:])" + alias + r"\s*::\s*" + _CTOR_TAIL))
    for m in _USE_RE.finditer(src):
        for path, name in _flatten_use_tree(m.group(1), []):
            if name == "_":
                continue
            n = re.escape(name)
            if path in (["Client"], ["blocking", "Client"]):
                pats.append(re.compile(r"(?<![\w:])" + n + r"\s*::\s*" + _CLIENT_CTORS + r"\s*\("))
            elif path in (["ClientBuilder"], ["blocking", "ClientBuilder"]):
                pats.append(re.compile(r"(?<![\w:])" + n + r"\s*::\s*" + _BUILDER_CTORS + r"\s*\("))
            elif path == ["blocking"]:
                pats.append(re.compile(r"(?<![\w:])" + n + r"\s*::\s*" + _CTOR_TAIL))
            elif path == ["*"] or path == ["blocking", "*"]:
                pats.append(
                    re.compile(r"(?<![\w:])Client\s*::\s*" + _CLIENT_CTORS + r"\s*\(")
                )
                pats.append(
                    re.compile(r"(?<![\w:])ClientBuilder\s*::\s*" + _BUILDER_CTORS + r"\s*\(")
                )
    return pats


# ---------------------------------------------------------------------------
# Scanning
# ---------------------------------------------------------------------------


def scan_source(rel: str, raw: str) -> list[Finding]:
    if rel in FACTORY_FILES:
        return []
    code = strip_test_items(strip_noise(raw))
    raw_lines = raw.splitlines()
    patterns = [_QUAL_RE] + construction_patterns(code)
    offsets: set[int] = set()
    for pat in patterns:
        for m in pat.finditer(code):
            offsets.add(m.start())
    findings = []
    for off in sorted(offsets):
        line = code.count("\n", 0, off) + 1
        here = raw_lines[line - 1] if line - 1 < len(raw_lines) else ""
        above = raw_lines[line - 2] if line >= 2 else ""
        if ESCAPE_RE.search(here) or ESCAPE_RE.search(above):
            continue
        findings.append(Finding(rel, line, here.strip()))
    return findings


def collect_sources(root: Path) -> dict[str, str]:
    base = root / SCAN_ROOT
    if not base.is_dir():
        raise SystemExit(f"scan root {base} does not exist — refusing to report an empty scan as clean")
    out: dict[str, str] = {}
    for p in sorted(base.rglob("*")):
        if p.is_file() and p.suffix.lower() == ".rs":
            out[p.relative_to(root).as_posix()] = p.read_text(encoding="utf-8", errors="replace")
    return out


def run_scan(root: Path) -> list[Finding]:
    findings: list[Finding] = []
    for rel, raw in collect_sources(root).items():
        findings += scan_source(rel, raw)
    return findings


# ---------------------------------------------------------------------------
# Baseline
# ---------------------------------------------------------------------------


class BaselineError(Exception):
    pass


def load_baseline(path: Path) -> dict[str, int]:
    if not path.exists():
        raise BaselineError(f"baseline {path} is missing")
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise BaselineError(f"baseline {path} is not valid JSON: {exc}") from exc
    if data.get("format") != BASELINE_FORMAT:
        raise BaselineError(
            f"baseline {path} declares format {data.get('format')!r}, expected {BASELINE_FORMAT}"
        )
    files = data.get("files")
    if not isinstance(files, dict):
        raise BaselineError(f"baseline {path} has no 'files' object")
    for k, v in files.items():
        if not isinstance(v, int) or isinstance(v, bool) or v < 1:
            raise BaselineError(f"baseline entry {k!r} must be a positive integer count, got {v!r}")
    return dict(files)


def write_baseline(path: Path, counts: dict[str, int]) -> None:
    data = {
        "_comment": [
            "reqwest clients constructed outside src-tauri/src/util/http.rs that are",
            "ACCEPTED, as a count per file. Enforced by",
            "scripts/check_reqwest_client_construction.py (a ratchet: more OR fewer",
            "than recorded fails). Plan",
            "2026-09-16-runner-long-lived-http-clients-freeze-dns-state-across-network-changes",
            "Wave B converted every site and emptied it; it must stay empty -- mark a",
            "genuine exception with `// reqwest-client-ok: <reason>` instead.",
        ],
        "format": BASELINE_FORMAT,
        "files": dict(sorted(counts.items())),
    }
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")


def count_by_file(findings: list[Finding]) -> dict[str, int]:
    counts: dict[str, int] = {}
    for f in findings:
        counts[f.path] = counts.get(f.path, 0) + 1
    return counts


def teach(lines: list[str], ci: bool) -> str:
    err = "::error::" if ci else ""
    body = [f"{err}reqwest client constructed outside the sanctioned factory:"]
    body += [f"{err}  {s}" for s in lines]
    body += [
        f"{err}",
        f"{err}WHY THIS IS A DEFECT: a reqwest Client owns its DNS resolver. A client built",
        f"{err}outside the factory gets reqwest's default GaiResolver, which spends a tokio",
        f"{err}blocking-pool thread per lookup (runner #918's starvation flood). The hickory-dns",
        f"{err}feature that once masked that snapshots DNS servers once per client and never",
        f"{err}recovers from a network change (2026-09-16: 90+ min coord outage); it is removed.",
        f"{err}",
        f"{err}FIX IT by starting from src-tauri/src/util/http.rs:",
        f"{err}  crate::util::http::client_builder()           // async; add your settings, .build()",
        f"{err}  crate::util::http::blocking_client_builder()  // reqwest::blocking",
        f"{err}  crate::util::http::ResettableClient::new(name, || client_builder()...build())",
        f"{err}                                                // process-lifetime clients",
        f"{err}",
        f"{err}A genuine exception carries `// reqwest-client-ok: <reason>` on the same line",
        f"{err}or the line above.",
    ]
    return "\n".join(body)


# ---------------------------------------------------------------------------
# Self-test
# ---------------------------------------------------------------------------


SELF_TEST_CASES: list[tuple[str, str, str, int]] = [
    ("planted qualified new", "src-tauri/src/a.rs", "fn f() { let c = reqwest::Client::new(); }", 1),
    ("qualified builder", "src-tauri/src/a.rs", "fn f() { reqwest::Client::builder().build(); }", 1),
    ("blocking builder", "src-tauri/src/a.rs", "fn f() { reqwest::blocking::Client::builder(); }", 1),
    ("ClientBuilder::new", "src-tauri/src/a.rs", "fn f() { reqwest::ClientBuilder::new(); }", 1),
    ("imported bare", "src-tauri/src/a.rs", "use reqwest::Client;\nfn f() { Client::new(); }", 1),
    ("imported alias", "src-tauri/src/a.rs", "use reqwest::{Client as Http, Url};\nfn f() { Http::builder(); }", 1),
    ("blocking module", "src-tauri/src/a.rs", "use reqwest::blocking;\nfn f() { blocking::Client::new(); }", 1),
    ("unrelated Client", "src-tauri/src/a.rs", "use crate::x::Client;\nfn f() { Client::new(); }", 0),
    ("EmbeddingClient", "src-tauri/src/a.rs", "use reqwest::Client;\nfn f() { EmbeddingClient::new(); }", 0),
    ("in a comment", "src-tauri/src/a.rs", "// reqwest::Client::new()\n/* reqwest::Client::builder() */ fn f() {}", 0),
    ("in a string", "src-tauri/src/a.rs", 'fn f() { let s = "reqwest::Client::new()"; }', 0),
    ("escape same line", "src-tauri/src/a.rs", "fn f() { reqwest::Client::new(); // reqwest-client-ok: fixture }", 0),
    ("escape line above", "src-tauri/src/a.rs", "fn f() {\n    // reqwest-client-ok: fixture\n    reqwest::Client::new();\n}", 0),
    ("escape needs reason", "src-tauri/src/a.rs", "fn f() {\n    reqwest::Client::new(); // reqwest-client-ok:\n}", 1),
    ("cfg(test) module", "src-tauri/src/a.rs", "fn f() {}\n#[cfg(test)]\nmod tests { fn t() { reqwest::Client::new(); } }", 0),
    ("after cfg(test) module", "src-tauri/src/a.rs", "#[cfg(test)]\nmod tests { fn t() {} }\nfn f() { reqwest::Client::new(); }", 1),
    ("factory exempt", "src-tauri/src/util/http.rs", "fn f() { reqwest::Client::builder(); }", 0),
    ("lifetime then call", "src-tauri/src/a.rs", "fn f<'a>(x: &'a str) { reqwest::Client::new(); }", 1),
    ("raw string", "src-tauri/src/a.rs", 'fn f() { let s = r#"reqwest::Client::new()"#; }', 0),
    # reqwest implements Default for all four client types: these compiled and
    # walked straight through the check until 2026-09-18.
    ("Client::default", "src-tauri/src/a.rs", "fn f() { let c = reqwest::Client::default(); }", 1),
    ("ClientBuilder::default", "src-tauri/src/a.rs", "fn f() { reqwest::ClientBuilder::default(); }", 1),
    (
        "blocking Client::default",
        "src-tauri/src/a.rs",
        "fn f() { reqwest::blocking::Client::default(); }",
        1,
    ),
    (
        "blocking ClientBuilder::default",
        "src-tauri/src/a.rs",
        "fn f() { reqwest::blocking::ClientBuilder::default(); }",
        1,
    ),
    (
        "imported bare default",
        "src-tauri/src/a.rs",
        "use reqwest::Client;\nfn f() { Client::default(); }",
        1,
    ),
    # A renamed crate: `_QUAL_RE` looks for the literal `reqwest` and sees none.
    ("crate alias", "src-tauri/src/a.rs", "use reqwest as rq;\nfn f() { rq::Client::new(); }", 1),
    (
        "crate alias blocking builder",
        "src-tauri/src/a.rs",
        "use reqwest as rq;\nfn f() { rq::blocking::Client::builder(); }",
        1,
    ),
    (
        "unrelated crate alias",
        "src-tauri/src/a.rs",
        "use other_crate as rq;\nfn f() { rq::Client::new(); }",
        0,
    ),
    # N11: a test helper gated on more than the bare `test` predicate is still
    # test code; a `not(test)` item is emphatically not.
    (
        "cfg(all(test, feature))",
        "src-tauri/src/a.rs",
        '#[cfg(all(test, feature = "x"))]\nmod t { fn f() { reqwest::Client::new(); } }',
        0,
    ),
    (
        "cfg(any(test, doc))",
        "src-tauri/src/a.rs",
        "#[cfg(any(test, doc))]\nmod t { fn f() { reqwest::Client::new(); } }",
        0,
    ),
    (
        "cfg(not(test)) is shipping code",
        "src-tauri/src/a.rs",
        "#[cfg(not(test))]\nfn f() { reqwest::Client::new(); }",
        1,
    ),
    (
        "cfg(all(not(test), unix))",
        "src-tauri/src/a.rs",
        "#[cfg(all(not(test), unix))]\nfn f() { reqwest::Client::new(); }",
        1,
    ),
    # N11: an array type carries a `;` BEFORE the item's opening brace, which
    # used to end the item there and leave its body scanned as shipping code.
    (
        "cfg(test) fn with an array parameter",
        "src-tauri/src/a.rs",
        "#[cfg(test)]\nfn t(buf: [u8; 4]) { reqwest::Client::new(); }",
        0,
    ),
    (
        "cfg(test) fn returning an array",
        "src-tauri/src/a.rs",
        "#[cfg(test)]\nfn t() -> [u8; 4] { reqwest::Client::new(); [0; 4] }",
        0,
    ),
    (
        "cfg(test) const with an array type",
        "src-tauri/src/a.rs",
        "#[cfg(test)]\nconst K: [u8; 4] = [0; 4];\nfn ship() { reqwest::Client::new(); }",
        1,
    ),
    (
        "cfg(test) item with a further attribute",
        "src-tauri/src/a.rs",
        "#[cfg(test)]\n#[tokio::test]\nasync fn t() { reqwest::Client::new(); }",
        0,
    ),
]


def self_test() -> int:
    failures = 0
    for name, rel, src, want in SELF_TEST_CASES:
        got = len(scan_source(rel, src))
        if got != want:
            failures += 1
            print(f"SELF-TEST FAIL: {name}: expected {want} finding(s), got {got}", file=sys.stderr)
    # End to end: a planted construction in a fixture tree fails the ratchet.
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        (root / SCAN_ROOT).mkdir(parents=True)
        (root / "scripts").mkdir()
        write_baseline(root / BASELINE_PATH, {})
        planted = root / SCAN_ROOT / "planted.rs"
        planted.write_text("fn f() { let _ = reqwest::Client::new(); }\n", encoding="utf-8")
        rc = check(root, ci=False, quiet=True)
        if rc == 0:
            failures += 1
            print("SELF-TEST FAIL: a planted reqwest::Client::new() passed the check", file=sys.stderr)
        write_baseline(root / BASELINE_PATH, {"src-tauri/src/planted.rs": 1})
        if check(root, ci=False, quiet=True) != 0:
            failures += 1
            print("SELF-TEST FAIL: a baselined site failed the check", file=sys.stderr)
        planted.write_text("fn f() {}\n", encoding="utf-8")
        if check(root, ci=False, quiet=True) == 0:
            failures += 1
            print("SELF-TEST FAIL: a stale (too-high) baseline entry passed the ratchet", file=sys.stderr)
    total = len(SELF_TEST_CASES) + 3
    if failures:
        print(f"SELF-TEST: {failures}/{total} case(s) failed", file=sys.stderr)
        return 1
    print(f"SELF-TEST OK: {total} case(s)")
    return 0


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def check(root: Path, *, ci: bool, quiet: bool = False) -> int:
    prefix = "::error::" if ci else ""

    def emit(msg: str) -> None:
        if not quiet:
            print(msg, file=sys.stderr)

    try:
        baseline = load_baseline(root / BASELINE_PATH)
    except BaselineError as exc:
        emit(f"{prefix}{exc}")
        emit(f"{prefix}FAILED: the baseline could not be read, so NOTHING is exempt.")
        return 1

    findings = run_scan(root)
    counts = count_by_file(findings)
    by_file: dict[str, list[Finding]] = {}
    for f in findings:
        by_file.setdefault(f.path, []).append(f)

    new_sites: list[str] = []
    problems: list[str] = []
    for path in sorted(set(counts) | set(baseline)):
        have = counts.get(path, 0)
        want = baseline.get(path, 0)
        if have > want:
            for f in by_file.get(path, []):
                new_sites.append(f"{f.path}:{f.line}  {f.snippet}")
            if want:
                problems.append(
                    f"{path} constructs {have} reqwest client(s); the baseline accepts {want}. "
                    f"A NEW construction was added (the sites above include the old ones — a "
                    f"per-file count cannot tell which is new)."
                )
        elif have < want:
            problems.append(
                f"{path} now constructs {have} reqwest client(s), the baseline records {want}. "
                f"Tighten it: set it to {have} in {BASELINE_PATH.as_posix()}"
                + (" (remove the entry)." if have == 0 else ".")
                + " The baseline is a ratchet."
            )

    if not new_sites and not problems:
        if not quiet:
            print(
                f"OK: every reqwest client is built through util::http or baselined "
                f"({len(findings)} baselined construction(s) across {len(counts)} file(s)).\n"
                f"scanned: {SCAN_ROOT.as_posix()}"
            )
        return 0
    if new_sites:
        emit(teach(new_sites, ci))
    for p in problems:
        emit(f"{prefix}{p}")
    emit(f"\n{prefix}FAILED: {len(new_sites)} unexempted site(s), {len(problems)} baseline problem(s).")
    return 1


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("--root", type=Path, default=DEFAULT_ROOT, help="repo root to scan")
    ap.add_argument("--ci", action="store_true", help="emit GitHub ::error:: annotations")
    ap.add_argument("--list", action="store_true", help="print every construction found")
    ap.add_argument("--list-roots", action="store_true", help="print the scan scope and exit")
    ap.add_argument(
        "--update-baseline",
        action="store_true",
        help="rewrite the baseline from the current tree (review the diff: it must only shrink)",
    )
    ap.add_argument("--self-test", action="store_true", help="run the built-in fixture tests")
    args = ap.parse_args()
    root: Path = args.root.resolve()

    if args.self_test:
        return self_test()

    if args.list_roots:
        print("SCANNED:")
        print(f"  {SCAN_ROOT.as_posix()}/**/*.rs  (minus test-cfg'd items, comments, literals)")
        print("EXEMPT — the sanctioned factory:")
        for f in sorted(FACTORY_FILES):
            print(f"  {f}")
        print("NOT SCANNED — crates/*: they cannot depend on the runner crate, so they")
        print("have no util::http to route through.")
        print()
        print("WHAT A GREEN RUN DOES AND DOES NOT CLAIM")
        print("  Green means every FIRST-PARTY client resolves through the OS DNS lane.")
        print("  It says NOTHING about clients built inside third-party crates on the")
        print("  same reqwest 0.13, which get reqwest's default GaiResolver:")
        print("    opentelemetry-otlp / opentelemetry-http — ROUTED: src-tauri/src/otel.rs")
        print("      injects util::http's client via .with_http_client(...).")
        print("    tauri, tauri-plugin-updater, postgresql_archive — NOT routed: no")
        print("      injection point exists, and none is claimed.")
        print("    sentry — rides a separate reqwest 0.12, which never had hickory-dns.")
        print("  The typed spelling `let c: reqwest::Client = Default::default();` needs")
        print("  type inference and is out of this scanner's reach; see the module docstring.")
        return 0

    if args.list:
        findings = run_scan(root)
        for f in findings:
            print(f"{f.path}:{f.line}  {f.snippet}")
        print(f"\n{len(findings)} construction(s) in {len(count_by_file(findings))} file(s)")
        return 0

    if args.update_baseline:
        findings = run_scan(root)
        counts = count_by_file(findings)
        write_baseline(root / BASELINE_PATH, counts)
        print(f"wrote {root / BASELINE_PATH}: {sum(counts.values())} construction(s) in {len(counts)} file(s)")
        return 0

    return check(root, ci=args.ci)


if __name__ == "__main__":
    sys.exit(main())
