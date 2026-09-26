#!/usr/bin/env python3
"""One typed reader for every coord / qontinui-web / runner response envelope.

Import it (the `sys.path` idiom `scripts/lint-git-scope.py` uses)::

    sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "lib"))
    from envelope import load, EnvelopeUnknown, non_empty_str

    env = load(resp.text, door="GET /coord/agent-findings", source=url)
    rows, count, prov = env.require_collection("findings")
    value, path_used, prov = env.require_first_present(
        ("data.value", "data.result.value", "data"), accept=non_empty_str)

or run it (the bash twin `scripts/lib/envelope.sh` calls this when `jq` is
absent)::

    python scripts/lib/envelope.py require --door <d> --key data.value        < file
    python scripts/lib/envelope.py require --door <d> --first-of a,b,c --accept non-empty-str --raw
    python scripts/lib/envelope.py require --door <d> --collection findings
    python scripts/lib/envelope.py require --door <d> --mcp-text --key hits   < call-result

# The defect this exists for

A wrong-key read of a JSON envelope yields the same value as an empty
answer. `data.get("hits", [])` on a body whose rows sit under `records`
is `[]`; `$r.data.result.value` on a runner that answers `data.value` is
`$null`; `.finding_id` off `{"posted": true, "finding": {"finding_id": …}}`
is nothing. Every one of those was then PUBLISHED as a verdict — "no
findings", "runner signed out", "write DROPPED" — by a session that had
read the wrong key of a correct body (dossiers
`wrong-envelope-key-reads-as-absence`, `empty-read-published-as-verdict`;
plan `2026-09-03-wrong-key-reads-cannot-yield-a-silent-zero`).

So the rule this module enforces, and that check #48
(`scripts/lint-envelope-reads.py`) refuses new code without:

    A key that is ABSENT is UNKNOWN, never a default. The reader RAISES
    (or exits 3 with an `UNKNOWN:` line on stderr and NOTHING on stdout)
    naming the door, the path, the keys that ARE present at the failing
    level, and where the body came from.

A caller that ignores the exit code still cannot capture an empty string
as a value, because nothing is written to stdout on the UNKNOWN arm.

# Where the key names live (ONE home — the docs point here)

The per-key notes that used to be scattered over `/unattended`,
`/manual-test-coord`, `/cleanup-steward`, `/plan-steward` and `/gate` are
folded into this table. Read a body through `require_key` /
`require_collection` / `require_first_present` with the path named here;
do not re-derive it from a summary elsewhere.

| Door | Shape | Read it as |
|---|---|---|
| runner `ApiResponse` (every `:9876` HTTP door) | `{success, data, error?}` | `require_key(p, "data.<field>")`; `success: false` carries `error` |
| runner UI-Bridge mint, eval door (LIVE) | `{success, data: {value, type}}` | `require_first_present(p, ("data.value", "data.result.value", "data"), accept=non_empty_str)` — live first, boxed second, the invoke door's bare-string `data` last |
| runner UI-Bridge mint, invoke door | `{success, data: "<jwt>"}` | same tuple; the predicate is what lets the bare string win over the always-present `data` object |
| runner UI-Bridge `page/evaluate` | `{success, data: {result, error?}}` | `require_first_present(p, ("data.result", "result"))`; read `data.error` before calling an empty result "no answer" |
| runner UI-Bridge `/health` | `{success, data: {responsive, connectedTabs, buildId, …}}` | `require_key(p, "data.connectedTabs")` |
| runner UI-Bridge `control/snapshot` | `{success, data: {elements}}` or bare `{elements}` | `require_first_present(p, ("data.elements", "elements"))` |
| coord `POST /coord/agent-findings` | `{posted, finding: {finding_id, …}}` or `{posted: false, reason}` | `require_key(p, "posted")` FIRST; the id is `finding.finding_id`, nested — never `finding_id` off the envelope. `201` stored a row, `200` did not |
| coord `GET /coord/agent-findings` | `{count, findings, limit, resource_keys_applied, resource_keys_truncated, available}` | `require_key(p, "available")` BEFORE `require_collection(p, "findings")`; `available: false` is UNKNOWN, not zero |
| coord `GET /coord/agent-gates` | `{gates, shown, total, truncated, count}` | `require_collection(p, "gates")`; `count` = this page (`shown` is its older name), `total` is unpaged |
| coord `GET /coord/agent-work-units` | `{work_units, limit, offset, count}` | `require_collection(p, "work_units")` |
| coord `GET /coord/agent-work-units/<slug>` | `{…, delivery, citations, citations_error?, delivery_error?}` | read `delivery` BEFORE `citations`; an empty `citations` is trustworthy only when `citations_error` is absent; `delivery.evidence_complete: false` means `shipped` could not be ESTABLISHED |
| coord `GET /coord/agent-work-units/<slug>/citations` | `{work_unit_id, citations, count}` or `{citations_error}` | `require_collection(p, "citations")` |
| coord `GET /coord/agent-prompt-documents` | `{documents, total, count}` | `require_collection(p, "documents")` |
| coord `GET /coord/agent-questions/agent-pending` | `{questions, count, shown, total, truncated}` | `require_collection(p, "questions")` |
| coord `GET /coord/sessions/worktrees` | `{tenantId, sessions, reapStatusCounts, count}` | `require_collection(p, "sessions")` (camelCase door) |
| coord `GET /coord/alerts` | `{alerts, total_count, count}` | `require_collection(p, "alerts")`; `total_count` is the unpaged total |
| coord MCP `tools/call` (JSON-RPC) | `{jsonrpc, id, result}` or `{jsonrpc, id, error}` | `require_key(p, "error")` first — its presence IS the answer; then `require_key(p, "result")` |
| `coord-revive.sh call <tool> '<json>'` stdout (the MCP `result`) | `{content: [{type, text: "<the tool's body as a JSON STRING>"}], isError?}` | `unwrap_mcp_text(p, door)` (CLI `--mcp-text`, bash `envelope_mcp_body`) FIRST — the tool's own `{hits, count, …}` sits one JSON decode deeper; `isError: true` and a non-JSON `text` are UNKNOWN. Then read the tool's row below |
| coord `coord_memory_search` | `{hits, vector_arm, live_row_count, query_echo, anchored_hits, anchored_hit_count, …}` — **no `count`** (measured live 2026-09-26; web `MemoryQueryResponse` declares none) | `require_key(p, "hits")` — NOT `require_collection`, which would answer UNKNOWN on the absent `count` every time. Beside a zero read `live_row_count` (this response's own tenant-wide denominator) and `vector_arm` (`skipped_no_embedding` is the normal case); a zero against `live_row_count > 0` is still a hypothesis until a control query returns rows |
| coord `coord_memory_overview` | `{row_count, corpus_complete, facets: {live_row_count, by_kind, by_scope} or null, …}` | `require_key(p, "facets.live_row_count")` — NESTED under `facets` (a top-level `live_row_count` read is the wrong-key defect); `facets: null` beside `corpus_complete: false` is UNKNOWN |
| web `GET /api/v1/memory/records` | `{records, next_cursor, count}` | `require_collection(p, "records")` |
| web `GET /api/v1/plan-library` | `{items, total, offset, limit, count}` | `require_collection(p, "items")`; `total` is the UNPAGED total |
| web `/plan-library/candidates`, `/open-followups` | `{items, total, count}` | `require_collection(p, "items")` |

`count` is REQUIRED by `require_collection`: absent is an `EnvelopeUnknown`
(the backend predates the phase that added it — that is unknown, not zero),
and a `count` that disagrees with `len(rows)` is an `EnvelopeUnknown` naming
both numbers. Pass `count_key="shown"` / `"total"` only for a door whose
page count is spelled that way AND whose semantics you have checked above.

# Server-side refusals this reader is the client half of

Phases 1-4 of the same plan make the servers refuse a wrong NAME on the way
in: coord's MCP dispatch answers an `isError` result naming the unknown
argument and the accepted set; coord's agent HTTP read doors answer `400
{"error", "unknown_params", "accepted_params"}` to an unknown query name;
qontinui-web bodies carry `extra="forbid"` and its query doors answer `422`
with the same `accepted_params` shape. Read a 400/422 of that shape as a
TYPO in the request, never as an outage.

# Provenance

Every value comes back with a `Provenance(door, source, fetched_at, mtime)`
so a verdict can say WHICH body it was read from and WHEN — the response-side
complement of the `/tmp` path-boundary plan
(`2026-09-02-msys-native-path-boundary-recurs-past-check-9-in-git-c`), whose
direction 5 names it. `mtime` is set when the body came from a file.

Stdlib only, no side effects at import, Python 3.9+.
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any, Callable, Optional, Sequence, Tuple

__all__ = [
    "Provenance",
    "EnvelopeUnknown",
    "Envelope",
    "load",
    "require_key",
    "require_collection",
    "require_first_present",
    "non_empty_str",
    "unwrap_mcp_text",
    "UNKNOWN_EXIT",
]

#: Exit status of the CLI (and of `envelope.sh`) on an UNKNOWN read. Not 1,
#: so a caller can tell "the key was absent" from "the reader crashed".
UNKNOWN_EXIT = 3

def _now_iso() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat()


@dataclass(frozen=True)
class Provenance:
    """Where a value was read from: the door, the source (a URL, a path,
    `stdin`), when it was fetched, and the file mtime when the source is a
    file."""

    door: str
    source: str
    fetched_at: str
    mtime: Optional[str] = None

    def render(self) -> str:
        out = f"source={self.source} at {self.fetched_at}"
        if self.mtime:
            out += f" (mtime {self.mtime})"
        return out


class EnvelopeUnknown(Exception):
    """A read that could not be answered. Never `None`, never `[]`, never "".

    Carries the door, the dotted path that failed, the keys PRESENT at the
    level where it failed (so the author sees the wrong-key case at once),
    and the provenance of the body. `str()` is the `UNKNOWN:` line the CLI
    prints on stderr.
    """

    def __init__(
        self,
        door: str,
        path: str,
        present_keys: Sequence[str],
        provenance: Provenance,
        detail: str = "",
        state: str = "absent",
    ) -> None:
        self.door = door
        self.state = state
        self.path = path
        self.present_keys = list(present_keys)
        self.provenance = provenance
        self.detail = detail
        super().__init__(self.__str__())

    def __str__(self) -> str:
        present = ", ".join(self.present_keys)
        out = (
            f"UNKNOWN: {self.door}: key `{self.path}` {self.state}; "
            f"present: [{present}]; {self.provenance.render()}"
        )
        if self.detail:
            out += f"; {self.detail}"
        return out


def non_empty_str(value: Any) -> bool:
    """The `accept` predicate for the mint read: a string with at least one
    non-whitespace character. `" "` is not a token: accepted, it would become
    `Authorization: Bearer  ` and read downstream as a refused credential."""
    return isinstance(value, str) and value.strip() != ""


def _present_keys(container: Any) -> list:
    if isinstance(container, dict):
        return sorted(str(k) for k in container.keys())
    if isinstance(container, list):
        return [f"<list of {len(container)}>"]
    return [f"<{type(container).__name__}>"]


def _walk(payload: Any, path: str) -> Tuple[bool, Any, str, Any]:
    """Follow a dotted path. Returns (found, value, failing_prefix, container).

    A segment that is all digits indexes a list; `.` alone is the root.
    `found` is False at the
    FIRST level the path cannot continue; `container` is the value at that
    level, so the caller can name the keys present there.
    """
    if path in ("", "."):
        # `.` (or the empty path) is the ROOT: a door that answers a bare list
        # is read as `require_first_present(p, ("data", "."))`.
        return True, payload, "", payload
    cur = payload
    walked: list = []
    for seg in path.split("."):
        if isinstance(cur, dict) and seg in cur:
            cur = cur[seg]
        elif isinstance(cur, list) and seg.isdigit() and int(seg) < len(cur):
            cur = cur[int(seg)]
        else:
            return False, None, ".".join(walked), cur
        walked.append(seg)
    return True, cur, path, cur


def _prov(door: str, provenance: Optional[Provenance]) -> Provenance:
    if provenance is not None:
        return provenance
    return Provenance(door=door, source="<in-memory>", fetched_at=_now_iso())


def require_key(
    payload: Any,
    path: str,
    door: str,
    provenance: Optional[Provenance] = None,
) -> Tuple[Any, Provenance]:
    """The value at dotted `path`, or `EnvelopeUnknown` naming the keys
    present at the level where the path stopped resolving.

    Presence is what is tested — a key holding `null` is PRESENT and `None`
    is returned. Absence is the only UNKNOWN.
    """
    prov = _prov(door, provenance)
    found, value, prefix, container = _walk(payload, path)
    if not found:
        detail = f"stopped at `{prefix or '<root>'}`" if prefix != path else ""
        raise EnvelopeUnknown(door, path, _present_keys(container), prov, detail)
    return value, prov


def require_collection(
    payload: Any,
    key: str,
    door: str,
    provenance: Optional[Provenance] = None,
    count_key: str = "count",
) -> Tuple[list, int, Provenance]:
    """The rows under `key` plus the sibling `count`, which must be present
    and equal to `len(rows)`. Both halves are UNKNOWN when absent: a page
    with no `count` came from a backend that predates the phase adding it,
    and that is not a zero.
    """
    prov = _prov(door, provenance)
    rows, _ = require_key(payload, key, door, prov)
    if not isinstance(rows, list):
        raise EnvelopeUnknown(
            door, key, _present_keys(rows), prov,
            f"`{key}` is a {type(rows).__name__}, not a list",
        )
    parent = key.rsplit(".", 1)[0] if "." in key else ""
    count_path = f"{parent}.{count_key}" if parent else count_key
    count, _ = require_key(payload, count_path, door, prov)
    if not isinstance(count, int) or isinstance(count, bool):
        raise EnvelopeUnknown(
            door, count_path, _present_keys(count), prov,
            f"`{count_path}` is a {type(count).__name__}, not an int",
        )
    if count != len(rows):
        raise EnvelopeUnknown(
            door, count_path, _present_keys(payload), prov,
            f"`{count_path}`={count} but `{key}` holds {len(rows)} row(s) — "
            f"the page is not self-consistent; do not act on either number",
        )
    return rows, count, prov


def require_first_present(
    payload: Any,
    paths: Sequence[str],
    door: str,
    *,
    accept: Optional[Callable[[Any], bool]] = None,
    provenance: Optional[Provenance] = None,
) -> Tuple[Any, str, Provenance]:
    """The first of `paths` that is present AND (when `accept` is given)
    whose value satisfies it. Returns `(value, path_used, provenance)`.

    Raises naming every path tried and, for each, whether it was absent or
    rejected. This is the mint read: `data` is present in EVERY shape the
    runner answers (as an object in two of them), so a plain first-present
    walk would always hand back the object — the predicate is what makes
    the tuple order mean something.
    """
    prov = _prov(door, provenance)
    if not paths:
        raise EnvelopeUnknown(door, "<no paths>", _present_keys(payload), prov,
                              "require_first_present called with no paths")
    tried: list = []
    for path in paths:
        found, value, prefix, container = _walk(payload, path)
        if not found:
            tried.append(f"`{path}` absent (present at `{prefix or '<root>'}`: "
                         f"[{', '.join(_present_keys(container))}])")
            continue
        if accept is not None and not accept(value):
            tried.append(f"`{path}` rejected ({type(value).__name__})")
            continue
        return value, path, prov
    raise EnvelopeUnknown(
        door, " | ".join(paths), _present_keys(payload), prov,
        "tried: " + "; ".join(tried),
    )


def unwrap_mcp_text(
    payload: Any,
    door: str,
    provenance: Optional[Provenance] = None,
) -> Any:
    """The tool's own body out of an MCP `tools/call` result.

    `coord-revive.sh call` prints the MCP `result` object, whose body is a
    JSON STRING at `content[0].text` - so `hits` is one decode deeper than any
    `require_*` reaches, and reading it straight off the result answers
    "absent" every time. The unwrap:

      * a full JSON-RPC envelope (`jsonrpc` present) descends into `result`;
        an `error` there is UNKNOWN naming it;
      * an `isError` that is present and not falsy (`true`, `"true"`, `1`, any
        non-empty value other than `false`/`"false"`/`0`/`null`) is UNKNOWN
        naming the tool's own error text - an error result is never a body
        with zero rows;
      * `content` present: it must hold EXACTLY ONE item, whose `text` is a
        string holding JSON, else UNKNOWN naming which half failed. A second
        item is refused rather than ignored: coord answers one text item, and
        a body spread over two is not one this reader can vouch for;
      * no `content` at all: the object itself falls through unchanged (a
        body that was never wrapped), so a later `require_*` still names the
        key it could not find.
    """
    prov = _prov(door, provenance)
    cur = payload
    if isinstance(cur, dict) and "jsonrpc" in cur:
        if "error" in cur:
            raise EnvelopeUnknown(door, "error", _present_keys(cur), prov,
                                  f"the JSON-RPC call failed: {json.dumps(cur['error'])[:300]}",
                                  state="present")
        cur, _ = require_key(cur, "result", door, prov)
    if not isinstance(cur, dict):
        return cur
    if "isError" in cur and cur["isError"] not in (False, None, 0, "", "false", "False"):
        text = ""
        content = cur.get("content")
        if isinstance(content, list) and content and isinstance(content[0], dict):
            text = str(content[0].get("text", ""))
        raise EnvelopeUnknown(door, "isError", _present_keys(cur), prov,
                              f"the tool answered an error, not a body: {text[:300]!r}",
                              state="is true")
    if "content" not in cur:
        return cur
    content = cur["content"]
    if isinstance(content, list) and len(content) > 1:
        raise EnvelopeUnknown(door, "content", _present_keys(cur), prov,
                              f"an MCP result carrying {len(content)} content items; exactly one is read",
                              state="holds more than one item")
    found, text, prefix, container = _walk(cur, "content.0.text")
    if not found or not isinstance(text, str):
        raise EnvelopeUnknown(door, "content.0.text", _present_keys(container), prov,
                              "an MCP result whose `content[0].text` is absent or not a string",
                              state="absent or not a string")
    try:
        return json.loads(text)
    except ValueError as exc:
        raise EnvelopeUnknown(door, "content.0.text", _present_keys(cur), prov,
                              f"`content[0].text` is not JSON ({exc.__class__.__name__}); "
                              f"first 120 chars: {text[:120]!r}", state="not JSON") from None


@dataclass(frozen=True)
class Envelope:
    """A parsed body plus its provenance; the method forms forward to the
    module functions with `door` and `provenance` filled in."""

    payload: Any
    provenance: Provenance

    @property
    def door(self) -> str:
        return self.provenance.door

    def mcp_body(self) -> "Envelope":
        """This envelope with the MCP `result` wrapper removed (`unwrap_mcp_text`)."""
        return Envelope(payload=unwrap_mcp_text(self.payload, self.door, self.provenance),
                        provenance=self.provenance)

    def require_key(self, path: str) -> Tuple[Any, Provenance]:
        return require_key(self.payload, path, self.door, self.provenance)

    def require_collection(self, key: str, count_key: str = "count") -> Tuple[list, int, Provenance]:
        return require_collection(self.payload, key, self.door, self.provenance, count_key=count_key)

    def require_first_present(
        self,
        paths: Sequence[str],
        *,
        accept: Optional[Callable[[Any], bool]] = None,
    ) -> Tuple[Any, str, Provenance]:
        return require_first_present(self.payload, paths, self.door,
                                     accept=accept, provenance=self.provenance)


def load(
    text_or_bytes: Any,
    *,
    door: str,
    source: str,
    mtime: Optional[str] = None,
) -> Envelope:
    """Parse a body. Unparseable JSON is an `EnvelopeUnknown` naming the
    first 120 bytes — never an empty dict, which would let every later read
    fail as "absent" against a body that was never JSON at all."""
    prov = Provenance(door=door, source=source, fetched_at=_now_iso(), mtime=mtime)
    if isinstance(text_or_bytes, (bytes, bytearray)):
        raw = bytes(text_or_bytes)
        text = raw.decode("utf-8", errors="replace")
    else:
        text = str(text_or_bytes)
        raw = text.encode("utf-8", errors="replace")
    try:
        payload = json.loads(text)
    except ValueError as exc:
        head = raw[:120].decode("utf-8", errors="replace")
        raise EnvelopeUnknown(
            door, "<json>", [], prov,
            f"body is not JSON ({exc.__class__.__name__}: {exc}); first 120 bytes: {head!r}",
        ) from None
    return Envelope(payload=payload, provenance=prov)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

_ACCEPTS = {
    "any": None,
    "non-empty-str": non_empty_str,
}


def _cli(argv: Sequence[str]) -> int:
    parser = argparse.ArgumentParser(
        prog="envelope.py",
        description="Typed envelope reads. Prints the value as JSON on stdout (exit 0); "
                    "on an UNKNOWN read prints NOTHING on stdout, an `UNKNOWN:` line on "
                    f"stderr, and exits {UNKNOWN_EXIT}.",
    )
    sub = parser.add_subparsers(dest="verb", required=True)
    req = sub.add_parser("require", help="read one key, the first of several, or a counted collection")
    req.add_argument("--door", required=True, help="the door the body came from (named in the UNKNOWN line)")
    req.add_argument("--source", default="stdin", help="provenance source label (default: stdin)")
    how = req.add_mutually_exclusive_group(required=True)
    how.add_argument("--key", help="dotted path, e.g. data.value (`.` is the whole body)")
    how.add_argument("--first-of", help="comma-separated dotted paths tried in order")
    how.add_argument("--collection", help="collection key whose sibling `count` must equal len(rows)")
    req.add_argument("--accept", choices=sorted(_ACCEPTS), default="any",
                     help="predicate a --first-of value must satisfy (non-empty-str for the mint read)")
    req.add_argument("--count-key", default="count", help="sibling count key for --collection (default: count)")
    req.add_argument("--mcp-text", action="store_true",
                     help="the body is an MCP tools/call result (coord-revive.sh call stdout): decode "
                          "content[0].text first; isError or a non-JSON text is UNKNOWN")
    req.add_argument("--raw", action="store_true",
                     help="print a string value bare (jq -r); non-strings are still JSON")
    args = parser.parse_args(list(argv))

    raw = sys.stdin.buffer.read()
    try:
        env = load(raw, door=args.door, source=args.source)
        if args.mcp_text:
            env = env.mcp_body()
        if args.key is not None:
            value, _ = env.require_key(args.key)
        elif args.first_of is not None:
            paths = tuple(p for p in args.first_of.split(",") if p != "")
            value, _, _ = env.require_first_present(paths, accept=_ACCEPTS[args.accept])
        else:
            value, _, _ = env.require_collection(args.collection, count_key=args.count_key)
    except EnvelopeUnknown as exc:
        sys.stderr.write(str(exc) + "\n")
        sys.stderr.flush()
        return UNKNOWN_EXIT

    if args.raw and isinstance(value, str):
        out = value
    else:
        out = json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    sys.stdout.buffer.write(out.encode("utf-8"))
    sys.stdout.buffer.write(b"\n")
    sys.stdout.flush()
    return 0


if __name__ == "__main__":
    sys.exit(_cli(sys.argv[1:]))
