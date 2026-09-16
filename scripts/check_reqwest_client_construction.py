#!/usr/bin/env python3
"""
Fail the build on a NEW reqwest client constructed outside the sanctioned factory.

WHAT THIS GATES
---------------
Constructing a `reqwest` client anywhere but `src-tauri/src/util/http.rs`:

    reqwest::Client::new()                    reqwest::Client::builder()
    reqwest::blocking::Client::new()          reqwest::blocking::Client::builder()
    reqwest::ClientBuilder::new()             reqwest::blocking::ClientBuilder::new()
    Client::new() / Client::builder()         -- where `Client` is imported from reqwest
                                                 (including `use reqwest::Client as Http;`
                                                 and `use reqwest::blocking;` + `blocking::Client::new()`)

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
from the baseline allows zero. Wave B of the plan converts the remaining sites
and empties the baseline.

A per-file COUNT cannot see a swap (one site converted and a new one added in
the same file in the same change). That hole is accepted for a baseline that is
being driven to empty; once it is empty, every construction outside the factory
fails outright.

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

_QUAL_RE = re.compile(
    r"(?<![\w:])reqwest\s*::\s*(?:blocking\s*::\s*)?"
    r"(?:Client\s*::\s*(?:new|builder)|ClientBuilder\s*::\s*new)\s*\("
)


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


_CFG_TEST_RE = re.compile(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]")


def strip_test_items(src: str) -> str:
    """Blank every item attributed `#[cfg(test)]` (on already-noise-stripped text)."""
    out = list(src)
    for m in _CFG_TEST_RE.finditer(src):
        j = m.end()
        # The item ends at its body's closing brace, or at `;` for a body-less
        # item (`mod tests;`, `use …;`), whichever comes first.
        brace = src.find("{", j)
        semi = src.find(";", j)
        if brace == -1 and semi == -1:
            continue
        if brace == -1 or (semi != -1 and semi < brace):
            end = semi
        else:
            end = _match_brace(src, brace)
        for k in range(m.start(), end + 1):
            if out[k] != "\n":
                out[k] = " "
    return "".join(out)


# ---------------------------------------------------------------------------
# Import resolution: which bare names mean a reqwest client type
# ---------------------------------------------------------------------------

_USE_RE = re.compile(r"\buse\s+(?:::)?reqwest\s*::\s*(.*?);", re.S)


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
    for m in _USE_RE.finditer(src):
        for path, name in _flatten_use_tree(m.group(1), []):
            if name == "_":
                continue
            n = re.escape(name)
            if path in (["Client"], ["blocking", "Client"]):
                pats.append(re.compile(r"(?<![\w:])" + n + r"\s*::\s*(?:new|builder)\s*\("))
            elif path in (["ClientBuilder"], ["blocking", "ClientBuilder"]):
                pats.append(re.compile(r"(?<![\w:])" + n + r"\s*::\s*new\s*\("))
            elif path == ["blocking"]:
                pats.append(
                    re.compile(
                        r"(?<![\w:])" + n + r"\s*::\s*"
                        r"(?:Client\s*::\s*(?:new|builder)|ClientBuilder\s*::\s*new)\s*\("
                    )
                )
            elif path == ["*"] or path == ["blocking", "*"]:
                pats.append(re.compile(r"(?<![\w:])Client\s*::\s*(?:new|builder)\s*\("))
                pats.append(re.compile(r"(?<![\w:])ClientBuilder\s*::\s*new\s*\("))
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
            "ACCEPTED for now, as a count per file. Enforced by",
            "scripts/check_reqwest_client_construction.py (a ratchet: more OR fewer",
            "than recorded fails). Wave B of plan",
            "2026-09-16-runner-long-lived-http-clients-freeze-dns-state-across-network-changes",
            "converts these to util::http::client_builder() and empties this file.",
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
        f"{err}WHY THIS IS A DEFECT: a reqwest Client owns its DNS resolver. With the",
        f"{err}hickory-dns feature it snapshots the DNS servers once and never recovers from",
        f"{err}a network change (2026-09-16: 90+ min coord outage); without it, GaiResolver",
        f"{err}spends a tokio blocking-pool thread per lookup (runner #918's starvation flood).",
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
        print(f"  {SCAN_ROOT.as_posix()}/**/*.rs  (minus #[cfg(test)] items, comments, literals)")
        print("EXEMPT — the sanctioned factory:")
        for f in sorted(FACTORY_FILES):
            print(f"  {f}")
        print("NOT SCANNED — crates/*: they cannot depend on the runner crate, so they")
        print("have no util::http to route through.")
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
