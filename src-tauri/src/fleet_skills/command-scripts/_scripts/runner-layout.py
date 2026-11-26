#!/usr/bin/env python3
"""Set the zone-grid layout of runner Terminal pages: the agent's /layout.

Usage: runner-layout.py <preset|N|fit> [--page ID[,ID..] | --all-pages]
                        [--trim-only] [--dry-run [--assume-closed ID[,ID..]]]
                        [--json] [--port N]
       runner-layout.py --selftest

<preset> is any preset the runner's own `/layout` accepts (single, split,
triptych, quad, 1-plus-4, six-pack, command-center, full-grid, flow-grid).
<N> is a window count: 1 single, 2 split, 3 triptych, 4 quad, 5-6 six-pack,
7-9 full-grid, 10+ flow-grid. `fit` means "the count of tabs on that page",
resolved per page, which is how a page is trimmed after its finished sessions
are closed: a runner older than auto-trim grows a grid by itself but never
shrinks one.

With no --page the active page (the one on screen) is set. --trim-only skips a
page unless the target has FEWER zones than the page's current grid, so it
never grows a page or re-applies its current layout; re-applying clears a
maximized zone, which is why a no-op is skipped rather than sent. It also skips
a page in tmux work mode (`workMode`), which has no grid to trim; a set without
--trim-only takes such a page back to a grid and the row says so.

--trim-only also reads who chose each page's layout (`get-page-layout`
`layoutOrigin`, served by a runner that trims on its own):
  auto      SKIPPED -- the runner chose it and shrinks it itself when tabs close;
  operator  SKIPPED -- the operator chose it, and nothing shrinks that choice;
  legacy    the page predates the field: trimmed as below, and sent with
            `origin: "auto"`, so the runner owns (and trims) it from then on;
  absent    a runner that does not trim on its own: trimmed as below.
With the runner's own trim turned off (`autoTrim` false) the runner shrinks
nothing, so an `auto` page is trimmed here too, again sent with
`origin: "auto"`. Every trim of a page that reports an origin carries
`origin: "auto"`: a set without one is an operator request, which the runner
records as `operator`, and an `operator` page is one this step never trims
again. The read-back requires `layoutOrigin` auto.
The served field is how the build is told apart, not a `gitSha` ancestry test.
Any other value is SKIPPED as unrecognised rather than guessed at.

--assume-closed ID[,ID..] (only with --dry-run; refused otherwise, exit 2)
previews the plan as it would stand after those terminals close: before each
page is planned, its tabCount is lowered by the listed terminals that
GET /terminals places on that page (an id or an id prefix; a terminal with no
pageId is on `default`). /triage-sessions' report mode passes the terminals its
close step would close, so its layout preview names the trims an act pass makes
after its closes. An id /terminals does not list lowers nothing and is named on
stderr. Plan 2026-11-07-triage-sessions-ten-gaps-from-an-every-page-act-run,
Phase 7.

Drives the runner's UI Bridge component `zone-layout-picker`
(`select-layout`, `get-page-layout`) over its HTTP API, and reads every set
back with `get-page-layout`. Stdlib only.

Exit codes: 0 every page set (or skipped) and verified, 1 a set failed or did
not verify, 2 the argument names no layout or --page names no page, 3 UNKNOWN:
the runner could not be read, or its build predates the page-targeted layout
actions (`ACTION_NOT_SUPPORTED`).
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


# What --trim-only does with each served `layoutOrigin` (absent: None). A
# runner that serves the field trims its own `auto` pages when tabs close, using
# its own `pickLayout` (3 tabs -> quad) rather than `layout_for_count` (3 ->
# triptych); skipping those pages is what keeps the two rules from fighting
# over one grid. A `legacy` page this script trims is handed to the runner with
# `origin: "auto"`, and the runner never re-trims a triptych at 3 tabs, because
# it trims only to strictly fewer zones.
ORIGIN_SKIPS = {
    "auto": "runner trims this page itself when tabs close (layoutOrigin auto)",
    "operator": "operator-chosen layout (layoutOrigin operator)",
}
TRIMMABLE_ORIGINS = (None, "legacy")


def runner_trims(state):
    """Does the runner shrink this page's grid by itself? Only a runner that
    reports `autoTrim` true does; absent (an older build) or false does not."""
    return state.get("autoTrim") is True


def send_origin(state, trim_only):
    """The `origin` to send with a set, or None to send none. A --trim-only set
    of a page that reports an origin (it is `auto` or `legacy`, since `plan`
    skips the rest) is the runner's kind of change, so it says so: sent without
    an origin it would be an operator request, which records the page as
    `operator`, and no later pass would trim it again. A trim always changes the
    layout id, so the runner does record `auto`. A set without --trim-only is
    the operator-style request the runner's `/layout` makes by default, and a
    runner that serves no `layoutOrigin` is never sent a field it may not
    know."""
    return "auto" if trim_only and state.get("layoutOrigin") in ("auto", "legacy") else None


def plan(arg, state, trim_only):
    """(status, settled, send, reason) for one page's read state, where status
    is SEND or SKIPPED. Pure, so the selftest can pin the trim rule."""
    tabs, current = state["tabCount"], state["layoutId"]
    target, send = resolve(arg, tabs)
    if target is None:
        return "SKIPPED", None, None, "no tabs on the page" if arg == "fit" else f"{arg!r} names no layout"
    settled = settles_at(target, tabs)
    origin = state.get("layoutOrigin")
    if trim_only and origin == "auto" and not runner_trims(state):
        origin = None  # the runner's trim is off: this page is ours to trim, as on an older build
    if trim_only and origin in ORIGIN_SKIPS:
        return "SKIPPED", settled, send, ORIGIN_SKIPS[origin]
    if trim_only and origin not in TRIMMABLE_ORIGINS:
        return "SKIPPED", settled, send, f"unrecognised layoutOrigin {origin!r}; not trimmed"
    if trim_only and state.get("workMode") == "tmux":
        # A tmux page shows one session and reads as single, so there is no grid
        # to trim, and any set would take it out of the mode the operator chose.
        return "SKIPPED", settled, send, "tmux mode has no grid to trim"
    if trim_only and settled == current:
        # Re-applying the current id cannot shrink it (a flow-grid keeps its
        # highest occupied zone) and would clear a maximized zone.
        return "SKIPPED", settled, send, f"already {current}"
    if trim_only and settled == FLOW_GRID:
        # flow-grid has a zone per tab and keeps one for its highest occupied
        # tile, so it never has fewer zones than a fixed preset holding the same
        # tabs: a page that settles there from another layout GROWS (full-grid
        # at 12 tabs, before the runner's own auto-grow, read as SEND here).
        return "SKIPPED", settled, send, f"{settled} would not trim {current}"
    if trim_only and zones(settled, tabs) >= state["zoneCount"]:
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


def terminal_pages():
    """{terminal id: page id} for every terminal GET /terminals lists; a terminal
    with no pageId is on `default`, as the runner UI shows it."""
    with urllib.request.urlopen(API + "/terminals", timeout=30) as r:
        body = json.loads(r.read())
    rows, _ = require_key(body, "data.terminals", door="GET /terminals")
    return {t["id"]: t.get("pageId") or "default" for t in rows}


def closing_counts(assumed, placed):
    """({page id: how many listed terminals it holds}, [ids that match no listed
    terminal]) for the --assume-closed ids `assumed` (each a full id or a
    prefix) over `placed` ({terminal id: page id}). A terminal two ids name
    counts once. Pure."""
    hit, unmatched = set(), []
    for a in assumed:
        found = [t for t in placed if t == a or t.startswith(a)]
        if not found:
            unmatched.append(a)
        hit.update(found)
    counts = {}
    for t in hit:
        counts[placed[t]] = counts.get(placed[t], 0) + 1
    return counts, unmatched


def assume_closed(state, closing):
    """`state` with its tabCount lowered by `closing` terminals (never below 0).
    Pure: plan() then decides on the page as it would stand after the closes."""
    return dict(state, tabCount=max(0, state["tabCount"] - closing)) if closing else state


def active_page():
    """The page on screen: get-page-layout with no pageId names it."""
    return (action("get-page-layout", {}) or {}).get("pageId")


def set_page(arg, page_id, trim_only, dry_run, closing=0):
    """One page. Returns a row: pageId, before, target, after, status
    (SET | SKIPPED | DRY-RUN | FAILED), reason. `closing` is how many of the
    page's terminals --assume-closed lists (dry runs only)."""
    row = {"pageId": page_id, "before": None, "target": None, "after": None, "status": "FAILED", "reason": ""}
    try:
        state = action("get-page-layout", {"pageId": page_id})
        state["layoutId"], state["zoneCount"], state["tabCount"]
    except PAGE_FAULTS as e:
        row["reason"] = f"get-page-layout refused: {e}"
        return row
    tmux = state.get("workMode") == "tmux"
    mode = ", tmux mode" if tmux else ""
    origin = state.get("layoutOrigin")
    owner = f", origin {origin}" if origin is not None else ""
    row["before"] = f"{state['layoutId']} ({state['zoneCount']} zones, {state['tabCount']} tabs{mode}{owner})"
    assumed = ""
    if closing:
        state = assume_closed(state, closing)
        assumed = f" (assuming {closing} closed: {state['tabCount']} tabs)"
    status, settled, send, reason = plan(arg, state, trim_only)
    reason += assumed
    row["target"] = settled
    if status == "SKIPPED":
        row["status"], row["reason"] = "SKIPPED", reason
        return row
    hand_over = send_origin(state, trim_only)
    handed = "; hands the page to the runner (origin auto)" if hand_over else ""
    if dry_run:
        row["status"], row["reason"] = ("DRY-RUN", f"would set {settled}"
                                        + ("; leaves tmux mode" if tmux else "") + handed)
        return row
    params = {"layoutId": send, "pageId": page_id}
    if hand_over:
        params["origin"] = hand_over
    try:
        res = action("select-layout", params)
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
        if (after["layoutId"] == want and (not trim_only or after["zoneCount"] < state["zoneCount"])
                and not (tmux and after.get("workMode") == "tmux")
                and (not hand_over or after.get("layoutOrigin") == hand_over)):
            row["after"], row["status"] = f"{want} ({after['zoneCount']} zones)", "SET"
            row["reason"] = ("read back; left tmux mode" if tmux else "read back") + handed
            return row
    if after is not None:
        row["after"] = f"{after['layoutId']} ({after['zoneCount']} zones)"
        row["reason"] = f"read back {row['after']}, wanted {want}" + (" with fewer zones" if trim_only else "")
        if tmux and after.get("workMode") == "tmux":
            row["reason"] = f"read back {row['after']}, still in tmux mode"
        elif hand_over and after["layoutId"] == want and after.get("layoutOrigin") != hand_over:
            row["reason"] = f"read back {row['after']}, but layoutOrigin {after.get('layoutOrigin')!r}, wanted {hand_over!r}"
    return row


def page_ids(arg):
    """The ids a --page value names, or None when --page was not given. An empty
    list is a --page that was given and names nothing, which is not the same."""
    return None if arg is None else [p.strip() for p in arg.split(",") if p.strip()]


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
        ("fit", {"layoutId": "single", "zoneCount": 1, "tabCount": 1, "workMode": "tmux"}, True, "SKIPPED", "single"),
        ("fit", {"layoutId": "single", "zoneCount": 1, "tabCount": 12, "workMode": "tmux"}, True, "SKIPPED", FLOW_GRID),
        # A flow-grid target never trims: from full-grid at 12 tabs it would grow.
        ("fit", {"layoutId": "full-grid", "zoneCount": 9, "tabCount": 12}, True, "SKIPPED", FLOW_GRID),
        ("full-grid", {"layoutId": "full-grid", "zoneCount": 9, "tabCount": 12, "layoutOrigin": "legacy", "autoTrim": True}, True, "SKIPPED", FLOW_GRID),
        ("fit", {"layoutId": "full-grid", "zoneCount": 9, "tabCount": 12}, False, "SEND", FLOW_GRID),
        ("fit", {"layoutId": "single", "zoneCount": 1, "tabCount": 4, "workMode": "tmux"}, False, "SEND", "quad"),
        ("fit", {"layoutId": "full-grid", "zoneCount": 9, "tabCount": 4, "workMode": "grid"}, True, "SEND", "quad"),
        # layoutOrigin (a runner that trims on its own): auto and operator pages
        # are the runner's and the operator's; a legacy page is trimmed as before.
        ("fit", {"layoutId": "full-grid", "zoneCount": 9, "tabCount": 4, "layoutOrigin": "auto", "autoTrim": True}, True, "SKIPPED", "quad"),
        # The runner's own trim off (or unreported): an auto page is ours to trim.
        ("fit", {"layoutId": "full-grid", "zoneCount": 9, "tabCount": 4, "layoutOrigin": "auto", "autoTrim": False}, True, "SEND", "quad"),
        ("fit", {"layoutId": "full-grid", "zoneCount": 9, "tabCount": 4, "layoutOrigin": "auto"}, True, "SEND", "quad"),
        ("fit", {"layoutId": "full-grid", "zoneCount": 9, "tabCount": 4, "layoutOrigin": "operator", "autoTrim": False}, True, "SKIPPED", "quad"),
        ("fit", {"layoutId": "full-grid", "zoneCount": 9, "tabCount": 4, "layoutOrigin": "operator"}, True, "SKIPPED", "quad"),
        ("fit", {"layoutId": "full-grid", "zoneCount": 9, "tabCount": 4, "layoutOrigin": "legacy"}, True, "SEND", "quad"),
        ("fit", {"layoutId": "full-grid", "zoneCount": 9, "tabCount": 4, "layoutOrigin": "mystery"}, True, "SKIPPED", "quad"),
        # Without --trim-only the origin decides nothing: an explicit set is sent.
        ("fit", {"layoutId": "full-grid", "zoneCount": 9, "tabCount": 4, "layoutOrigin": "operator"}, False, "SEND", "quad"),
    ]
    for arg, state, trim, want_status, want_settled in plans:
        got = plan(arg, state, trim)[:2]
        if got != (want_status, want_settled):
            print(f"FAIL plan({arg!r}, {state}, trim={trim}) = {got}, want {(want_status, want_settled)}")
            failures += 1
    origins = [  # (state, trim_only, origin a set sends)
        ({"layoutOrigin": "legacy", "autoTrim": True}, True, "auto"),
        ({"layoutOrigin": "legacy", "autoTrim": False}, True, "auto"),
        ({"layoutOrigin": "auto", "autoTrim": False}, True, "auto"),
        ({"layoutOrigin": "legacy"}, False, None),  # an explicit set is the operator's
        ({}, True, None),  # an older runner is never sent the field
    ]
    for state, trim, want in origins:
        if send_origin(state, trim) != want:
            print(f"FAIL send_origin({state}, trim={trim}) = {send_origin(state, trim)!r}, want {want!r}")
            failures += 1
    # The invariant the rows above sample: no --trim-only SEND of a page that
    # reports an origin goes out without one, or the page turns `operator`.
    for origin in ("auto", "legacy", "operator", "mystery"):
        for trim_on in (True, False):
            st = {"layoutId": "full-grid", "zoneCount": 9, "tabCount": 4, "layoutOrigin": origin, "autoTrim": trim_on}
            for arg in ("fit", "4", "Quad"):
                if plan(arg, st, True)[0] == "SEND" and send_origin(st, True) != "auto":
                    print(f"FAIL a --trim-only SEND of a {origin} page ({arg!r}) carries no origin")
                    failures += 1
    for arg, want in [(None, None), ("", []), (" , ", []), ("a", ["a"]), ("a, b,", ["a", "b"])]:
        if page_ids(arg) != want:
            print(f"FAIL page_ids({arg!r}) = {page_ids(arg)!r}, want {want!r}")
            failures += 1
    # --assume-closed (plan triage ten-gaps Phase 7): the count is lowered per page before plan() decides.
    placed = {"t-aaaa1": "p1", "t-aaaa2": "p1", "t-bbbb1": "p2", "t-cccc1": "default"}
    assumed_cases = [  # (ids, expected counts, expected unmatched)
        (["t-aaaa1", "t-bbbb1"], {"p1": 1, "p2": 1}, []),
        (["t-aaaa"], {"p1": 2}, []),  # a prefix names both of p1's
        (["t-aaaa1", "t-aaaa1", "t-aaaa"], {"p1": 2}, []),  # a terminal counts once
        (["t-cccc1", "t-zzzz"], {"default": 1}, ["t-zzzz"]),
        ([], {}, []),
    ]
    for ids, want_counts, want_unmatched in assumed_cases:
        got = closing_counts(ids, placed)
        if got != (want_counts, want_unmatched):
            print(f"FAIL closing_counts({ids}) = {got}, want {(want_counts, want_unmatched)}")
            failures += 1
    assumed_plans = [  # (state, closing, expected status, expected settled)
        # 9-zone full-grid holding 5 tabs: as it stands it settles at six-pack; with 2 closed, at triptych
        ({"layoutId": "full-grid", "zoneCount": 9, "tabCount": 5}, 0, "SEND", "six-pack"),
        ({"layoutId": "full-grid", "zoneCount": 9, "tabCount": 5}, 2, "SEND", "triptych"),
        # a quad of 4 tabs is not trimmed as it stands; with 2 closed it trims to split
        ({"layoutId": "quad", "zoneCount": 4, "tabCount": 4}, 0, "SKIPPED", "quad"),
        ({"layoutId": "quad", "zoneCount": 4, "tabCount": 4}, 2, "SEND", "split"),
        # every tab closing leaves nothing to fit, and the count never goes below 0
        ({"layoutId": "quad", "zoneCount": 4, "tabCount": 2}, 3, "SKIPPED", None),
    ]
    for state, closing, want_status, want_settled in assumed_plans:
        got = plan("fit", assume_closed(state, closing), True)[:2]
        if got != (want_status, want_settled):
            print(f"FAIL plan(fit, assume_closed({state}, {closing})) = {got}, want {(want_status, want_settled)}")
            failures += 1
    if assume_closed({"tabCount": 2}, 3)["tabCount"] != 0 or assume_closed({"tabCount": 2}, 0) != {"tabCount": 2}:
        print("FAIL assume_closed must floor at 0 and leave an unlisted page as read")
        failures += 1
    # --assume-closed without --dry-run is refused before any runner read (exit 2).
    import contextlib
    import io
    saved = sys.argv
    try:
        for argv, want in ((["fit", "--all-pages", "--assume-closed", "t1"], 2),
                           (["fit", "--all-pages", "--trim-only", "--assume-closed", "t1"], 2),
                           (["fit", "--all-pages", "--dry-run", "--assume-closed", " , "], 2)):
            sys.argv = ["runner-layout.py"] + argv
            with contextlib.redirect_stderr(io.StringIO()), contextlib.redirect_stdout(io.StringIO()):
                got = main()
            if got != want:
                print(f"FAIL main({argv}) = {got}, want {want} (refused before any runner read)")
                failures += 1
    finally:
        sys.argv = saved
    for bad in ("\u00b2", "\u0664"):
        if resolve(bad, 3)[0] is not None:
            print(f"FAIL resolve({bad!r}) accepted a non-ASCII digit")
            failures += 1
    print(f"selftest: {len(expect)} count cases, {len(cases)} resolve cases, {len(plans)} plan cases, "
          f"{len(origins)} origin cases, {len(assumed_cases) + len(assumed_plans) + 4} assume-closed cases, "
          f"{failures} failure(s)")
    return 1 if failures else 0


def main():
    for stream in (sys.stdout, sys.stderr):  # a piped Windows console is cp1252
        try:
            stream.reconfigure(encoding="utf-8", errors="backslashreplace")
        except (AttributeError, ValueError, OSError):
            pass
    if "--selftest" in sys.argv[1:]:
        return selftest()
    ap = argparse.ArgumentParser()
    ap.add_argument("layout", help="a preset id, a window count, or `fit`")
    g = ap.add_mutually_exclusive_group()
    g.add_argument("--page", help="page id(s), comma-separated (default: the active page)")
    g.add_argument("--all-pages", action="store_true")
    ap.add_argument("--trim-only", action="store_true", help="only set a page whose grid would shrink")
    ap.add_argument("--dry-run", action="store_true", help="read and report; set nothing")
    ap.add_argument("--assume-closed", metavar="ID[,ID..]",
                    help="with --dry-run only: plan each page as if these terminals were closed")
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
    assumed = page_ids(a.assume_closed)
    if assumed is not None and not a.dry_run:
        # A preview only: a real set over a count the runner does not hold would
        # re-grid a page for tabs that are still open.
        print("--assume-closed previews a plan and is refused without --dry-run", file=sys.stderr)
        return 2
    if assumed == []:
        print(f"--assume-closed {a.assume_closed!r} names no terminal", file=sys.stderr)
        return 2
    named = page_ids(a.page)
    if named == []:
        # A blank `--page "$PAGE"` must not fall through to the page on screen:
        # that re-grids what the operator is looking at, not the page meant.
        print(f"--page {a.page!r} names no page", file=sys.stderr)
        return 2
    try:
        targets = pages() if a.all_pages else named or [active_page()]
        if None in targets:
            raise RuntimeError("the active page could not be read; pass --page")
        counts = {}
        if assumed:
            counts, unmatched = closing_counts(assumed, terminal_pages())
            if unmatched:
                print(f"--assume-closed: no listed terminal matches {', '.join(unmatched)}; it lowers nothing",
                      file=sys.stderr)
        rows = [set_page(a.layout, p, a.trim_only, a.dry_run, counts.get(p, 0)) for p in targets]
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
