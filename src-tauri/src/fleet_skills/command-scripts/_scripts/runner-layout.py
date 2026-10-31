#!/usr/bin/env python3
"""Set the zone-grid layout of runner Terminal pages: the agent's /layout.

Usage: runner-layout.py <preset|N|fit> [--page ID[,ID..] | --all-pages]
                        [--trim-only] [--dry-run] [--json] [--port N]
       runner-layout.py --selftest

<preset> is any preset the runner's own `/layout` accepts (single, split,
triptych, quad, 1-plus-4, six-pack, command-center, full-grid, flow-grid).
<N> is a window count: 1 single, 2 split, 3 triptych, 4 quad, 5-6 six-pack,
7-9 full-grid, 10+ flow-grid. `fit` means "the count of tabs on that page",
resolved per page, which is how a page is trimmed after its finished sessions
are closed: the runner grows a grid by itself but never shrinks one.

With no --page the active page (the one on screen) is set. --trim-only skips a
page unless the target has FEWER zones than the page's current grid, so it
never grows a page or re-applies its current layout; re-applying clears a
maximized zone, which is why a no-op is skipped rather than sent.

Drives the runner's UI Bridge component `zone-layout-picker`
(`select-layout`, `get-page-layout`) over its HTTP API, and reads every set
back with `get-page-layout`. Stdlib only.

Exit codes: 0 every page set (or skipped) and verified, 1 a set failed or did
not verify, 3 UNKNOWN: the runner could not be read, or its build predates the
page-targeted layout actions (`ACTION_NOT_SUPPORTED`).
"""
import argparse
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

sys.dont_write_bytecode = True
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "lib"))
from envelope import EnvelopeUnknown, require_key  # noqa: E402

API = "http://127.0.0.1:" + (os.environ.get("QONTINUI_RUNNER_API_PORT") or "9876")
COMPONENT = "zone-layout-picker"
FLOW_GRID = "flow-grid"
# Zone count of each fixed preset (qontinui-runner useZoneLayout.ts
# LAYOUT_PRESETS). flow-grid has one zone per tab, so it is not listed.
ZONES = {"single": 1, "split": 2, "triptych": 3, "quad": 4, "1-plus-4": 3, "six-pack": 6,
         "command-center": 9, "full-grid": 9}


class Unsupported(Exception):
    """The running runner build has no page-targeted layout action."""


# Anything a single page's read or write can raise, short of Unsupported: the
# page reads FAILED and the run goes on to the next one.
PAGE_FAULTS = (RuntimeError, KeyError, TypeError, ValueError, OSError, urllib.error.URLError, EnvelopeUnknown)


def pick_layout(n):
    """The runner's `pickLayout` (useZoneLayout.ts): the layout auto-grow picks
    when a page's tabs overflow its grid. It differs from layoutForCount at 3."""
    if n >= 10:
        return FLOW_GRID
    if n >= 7:
        return "full-grid"
    if n >= 5:
        return "six-pack"
    if n >= 3:
        return "quad"
    return "split" if n >= 2 else "single"


def layout_for_count(n):
    """The runner's `layoutForCount` (useZoneLayout.ts): what a window count
    names. The runner resolves the number itself on a real set; these mirrors
    only predict, so a dry run and --trim-only can decide without a write."""
    return "triptych" if n == 3 else pick_layout(n)


def zones(layout_id, tabs):
    """Fixed preset zone count. A flow-grid's is not predictable here: the
    runner sizes it to the highest occupied zone, which closes leave behind."""
    return max(tabs, 1) if layout_id == FLOW_GRID else ZONES.get(layout_id, 0)


def settles_at(target, tabs):
    """The layout the runner's auto-grow leaves after setting `target` on a page
    of `tabs` tabs (computeAutoGrowLayoutId: grow only when the tabs overflow)."""
    if target == FLOW_GRID or tabs <= zones(target, tabs):
        return target
    grown = pick_layout(tabs)
    return grown if zones(grown, tabs) > zones(target, tabs) else target


def plan(arg, state, trim_only):
    """(status, settled, send, reason) for one page's read state, where status
    is SEND or SKIPPED. Pure, so the selftest can pin the trim rule."""
    tabs, current = state["tabCount"], state["layoutId"]
    target, send = resolve(arg, tabs)
    if target is None:
        return "SKIPPED", None, None, "no tabs on the page" if arg == "fit" else f"{arg!r} names no layout"
    settled = settles_at(target, tabs)
    if trim_only and settled == current:
        # Re-applying the current id cannot shrink it (a flow-grid keeps its
        # highest occupied zone) and would clear a maximized zone.
        return "SKIPPED", settled, send, f"already {current}"
    if trim_only and settled != FLOW_GRID and zones(settled, tabs) >= state["zoneCount"]:
        return "SKIPPED", settled, send, f"{settled} would not trim {current}"
    return "SEND", settled, send, ""


def resolve(arg, tabs):
    """(layout id the argument names for a page of `tabs` tabs, the argument
    to send), or (None, None) when it names none."""
    a = arg.strip().lower()
    if a == "fit":
        return (layout_for_count(tabs), str(tabs)) if tabs >= 1 else (None, None)
    if re.fullmatch(r"[0-9]+", a):
        return (layout_for_count(int(a)), a) if int(a) >= 1 else (None, None)
    a = {"sixpack": "six-pack", "fullgrid": "full-grid", "flowgrid": FLOW_GRID}.get(a, a)
    return (a, a) if a in ZONES or a == FLOW_GRID else (None, None)


def action(action_id, params, timeout=30):
    """The `result` of one zone-layout-picker action. Raises Unsupported on a
    build without it, RuntimeError on any other refusal."""
    req = urllib.request.Request(f"{API}/ui-bridge/control/component/{COMPONENT}/action/{action_id}",
                                 method="POST", data=json.dumps(params).encode(),
                                 headers={"Content-Type": "application/json"})
    try:
        body = json.loads(urllib.request.urlopen(req, timeout=timeout).read() or b"{}")
    except urllib.error.HTTPError as e:
        body = json.loads(e.read() or b"{}")
    if not body.get("success"):
        if body.get("code") == "ACTION_NOT_SUPPORTED":
            raise Unsupported(body.get("error") or action_id)
        raise RuntimeError(body.get("error") or str(body)[:200])
    data, _ = require_key(body, "data", door=f"POST zone-layout-picker/{action_id}")
    if isinstance(data, dict) and data.get("success") is False:
        raise RuntimeError(data.get("error") or str(data)[:200])
    result, _ = require_key(body, "data.result", door=f"POST zone-layout-picker/{action_id}")
    return result


def pages():
    """[pageId] for every terminal page the runner knows, from GET /terminal-pages."""
    with urllib.request.urlopen(API + "/terminal-pages", timeout=30) as r:
        body = json.loads(r.read())
    page_rows, _ = require_key(body, "data.pages", door="GET /terminal-pages")
    return [p["pageId"] for p in page_rows]


def active_page():
    """The page on screen: get-page-layout with no pageId names it."""
    return (action("get-page-layout", {}) or {}).get("pageId")


def set_page(arg, page_id, trim_only, dry_run):
    """One page. Returns a row: pageId, before, target, after, status
    (SET | SKIPPED | DRY-RUN | FAILED), reason."""
    row = {"pageId": page_id, "before": None, "target": None, "after": None, "status": "FAILED", "reason": ""}
    try:
        state = action("get-page-layout", {"pageId": page_id})
        state["layoutId"], state["zoneCount"], state["tabCount"]
    except PAGE_FAULTS as e:
        row["reason"] = f"get-page-layout refused: {e}"
        return row
    row["before"] = f"{state['layoutId']} ({state['zoneCount']} zones, {state['tabCount']} tabs)"
    status, settled, send, reason = plan(arg, state, trim_only)
    row["target"] = settled
    if status == "SKIPPED":
        row["status"], row["reason"] = "SKIPPED", reason
        return row
    if dry_run:
        row["status"], row["reason"] = "DRY-RUN", f"would set {settled}"
        return row
    try:
        res = action("select-layout", {"layoutId": send, "pageId": page_id})
    except PAGE_FAULTS as e:
        row["reason"] = f"select-layout refused: {e}"
        return row
    want = (res.get("layoutId") if isinstance(res, dict) else None) or settled
    # The set is a React state update: read it back once it has rendered. A
    # trim counts only when the grid actually has fewer zones than before.
    after = None
    for _ in range(10):
        time.sleep(0.5)
        try:
            after = action("get-page-layout", {"pageId": page_id})
        except PAGE_FAULTS as e:
            row["reason"] = f"set sent; read-back refused: {e}"
            return row
        if after["layoutId"] == want and (not trim_only or after["zoneCount"] < state["zoneCount"]):
            row["after"], row["status"] = f"{want} ({after['zoneCount']} zones)", "SET"
            row["reason"] = "read back"
            return row
    if after is not None:
        row["after"] = f"{after['layoutId']} ({after['zoneCount']} zones)"
        row["reason"] = f"read back {row['after']}, wanted {want}" + (" with fewer zones" if trim_only else "")
    return row


def selftest():
    """The count mapping (pinned to the runner's layoutForCount test table) and
    the trim decision. Reads nothing."""
    failures = 0
    expect = {1: "single", 2: "split", 3: "triptych", 4: "quad", 5: "six-pack", 6: "six-pack",
              7: "full-grid", 9: "full-grid", 10: FLOW_GRID, 25: FLOW_GRID}
    for n, want in expect.items():
        failures += layout_for_count(n) != want
        failures += zones(layout_for_count(n), n) < n
    cases = [  # (arg, tabs, expected target)
        ("fit", 4, "quad"), ("fit", 0, None), ("4", 9, "quad"), ("0", 3, None), ("sixpack", 2, "six-pack"),
        ("Full-Grid", 1, "full-grid"), ("flowgrid", 12, FLOW_GRID), ("bogus", 3, None), ("2.5", 3, None),
    ]
    for arg, tabs, want in cases:
        got = resolve(arg, tabs)[0]
        if got != want:
            print(f"FAIL resolve({arg!r}, {tabs}) = {got!r}, want {want!r}")
            failures += 1
    plans = [  # (arg, {layoutId, zoneCount, tabCount}, trim_only, expected status, expected settled)
        ("fit", {"layoutId": "full-grid", "zoneCount": 9, "tabCount": 4}, True, "SEND", "quad"),
        ("fit", {"layoutId": "quad", "zoneCount": 4, "tabCount": 4}, True, "SKIPPED", "quad"),
        ("2", {"layoutId": "quad", "zoneCount": 4, "tabCount": 3}, True, "SKIPPED", "quad"),  # grows back to quad
        ("1", {"layoutId": "quad", "zoneCount": 4, "tabCount": 3}, True, "SKIPPED", "quad"),
        ("fit", {"layoutId": "quad", "zoneCount": 4, "tabCount": 3}, True, "SEND", "triptych"),
        ("fit", {"layoutId": FLOW_GRID, "zoneCount": 14, "tabCount": 12}, True, "SKIPPED", FLOW_GRID),
        ("fit", {"layoutId": FLOW_GRID, "zoneCount": 14, "tabCount": 8}, True, "SEND", "full-grid"),
        ("fit", {"layoutId": "split", "zoneCount": 2, "tabCount": 5}, True, "SKIPPED", "six-pack"),
        ("fit", {"layoutId": "split", "zoneCount": 2, "tabCount": 5}, False, "SEND", "six-pack"),
        ("fit", {"layoutId": "single", "zoneCount": 1, "tabCount": 0}, True, "SKIPPED", None),
    ]
    for arg, state, trim, want_status, want_settled in plans:
        got = plan(arg, state, trim)[:2]
        if got != (want_status, want_settled):
            print(f"FAIL plan({arg!r}, {state}, trim={trim}) = {got}, want {(want_status, want_settled)}")
            failures += 1
    for bad in ("\u00b2", "\u0664"):
        if resolve(bad, 3)[0] is not None:
            print(f"FAIL resolve({bad!r}) accepted a non-ASCII digit")
            failures += 1
    print(f"selftest: {len(expect)} count cases, {len(cases)} resolve cases, {len(plans)} plan cases, "
          f"{failures} failure(s)")
    return 1 if failures else 0


def main():
    if "--selftest" in sys.argv[1:]:
        return selftest()
    ap = argparse.ArgumentParser()
    ap.add_argument("layout", help="a preset id, a window count, or `fit`")
    g = ap.add_mutually_exclusive_group()
    g.add_argument("--page", default="", help="page id(s), comma-separated (default: the active page)")
    g.add_argument("--all-pages", action="store_true")
    ap.add_argument("--trim-only", action="store_true", help="only set a page whose grid would shrink")
    ap.add_argument("--dry-run", action="store_true", help="read and report; set nothing")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--port", help="runner API port (default $QONTINUI_RUNNER_API_PORT or 9876)")
    a = ap.parse_args()
    global API
    if a.port:
        API = f"http://127.0.0.1:{a.port}"
    if resolve(a.layout, 1)[0] is None:
        print(f"{a.layout!r} is not a layout: use a preset id, a window count of 1 or more, or `fit`",
              file=sys.stderr)
        return 2
    try:
        targets = pages() if a.all_pages else [p for p in a.page.split(",") if p] or [active_page()]
        if None in targets:
            raise RuntimeError("the active page could not be read; pass --page")
        rows = [set_page(a.layout, p, a.trim_only, a.dry_run) for p in targets]
    except Unsupported as e:
        print(f"UNKNOWN: this runner build has no page-targeted layout action ({e}); "
              "it predates numeric /layout. Use the runner's own /layout command bar.", file=sys.stderr)
        return 3
    except (urllib.error.URLError, OSError, ValueError, KeyError, RuntimeError, EnvelopeUnknown) as e:
        print(f"UNKNOWN: could not read the runner at {API}: {e}", file=sys.stderr)
        return 3
    if a.json:
        print(json.dumps(rows, indent=1))
    else:
        for r in rows:
            print(f"  {r['status']:<8} {r['pageId'][:8]}  {r['before'] or '?'} -> "
                  f"{r['after'] or r['target'] or '-'}  - {r['reason']}")
    return 1 if any(r["status"] == "FAILED" for r in rows) else 0


if __name__ == "__main__":
    sys.exit(main())
