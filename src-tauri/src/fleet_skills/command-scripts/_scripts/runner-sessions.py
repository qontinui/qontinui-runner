#!/usr/bin/env python3
"""Report every open session in the runner, grouped by Terminal-page tab.

Usage: runner-sessions.py [--idle-min N] [--json] [--port N]

One row per terminal: the Claude account it runs under, the session's name as
Claude Code's /resume shows it, and whether it is ACTIVE (a turn in progress) or
INACTIVE (idle at its prompt, waiting for input, or exited). The runner reads,
process facts and screen checks are close-finished-sessions.py's, imported from
beside this file, so the overlord commands and this report read tabs, accounts
and screens the same way. Strictly read-only: it types nothing, closes nothing,
and samples nothing over time.

ACTIVE needs evidence of a turn in progress: a running turn on screen ('esc to
interrupt' after the last turn end), or a user/assistant transcript record
newer than --idle-min. Background tool shells alone (a watcher left running
after the turn ended) do not make a session active; they are named in the
detail. A pane this script cannot read is UNKNOWN, never INACTIVE.

The session's own declared status (coord `working` / `waiting_human` /
`finished`) is the work axis, not liveness: it says whether the work is done,
not whether a turn is running, so it is shown in the detail and decides nothing.

Exit codes: 0 report printed, 3 UNKNOWN: the runner API could not be read.
"""
import argparse
import glob
import importlib.util
import json
import os
import sys
import time

# The helper runs from a provisioned skill directory, which must not collect a __pycache__.
sys.dont_write_bytecode = True
_here = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("cfs", os.path.join(_here, "close-finished-sessions.py"))
cfs = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cfs)

ACTIVE, INACTIVE, UNKNOWN = "active", "inactive", "unknown"


def judge(f, idle_min):
    """(state, detail) from one pane's facts. Pure, so --selftest can pin it.

    `f` keys: claude (True / False / None = unreadable, with `unreadable` the
    reason), other (a non-shell process when claude is absent), exited (the
    detail for a pane with no claude), unbound (why the registry row is not the
    running session, or ""), shells, screen_busy (None = unreadable), waiting
    (why the prompt is not empty, or ""), status (declared status or None),
    note (a caveat about which registry row is shown, or ""), last_activity_ms,
    now_ms."""
    if f["claude"] is None:
        return UNKNOWN, f["unreadable"]
    if not f["claude"]:
        if f["other"]:
            return UNKNOWN, "no claude process, but the pane runs " + f["other"]
        return INACTIVE, f["exited"]
    if f["unbound"]:
        return UNKNOWN, f["unbound"]
    last = f["last_activity_ms"]
    idle = None if last is None else (f["now_ms"] - last) / 60000
    age = "no user/assistant record in the transcript" if idle is None else \
        f"last activity {cfs.fmt_age(last)} ago"
    tail = [age]
    if f["shells"]:
        tail.append(f"{f['shells']} background tool shell(s)")
    if f["status"]:
        tail.append(f"status {f['status']}")
    if f["note"]:
        tail.append(f["note"])
    work = []
    if f["screen_busy"]:
        work.append("turn running on screen")
    if idle is not None and idle < idle_min:
        work.append(f"activity {idle:.0f}m ago")
    if work:
        return ACTIVE, "; ".join(work + tail[1:])
    if f["screen_busy"] is None and idle is None:
        return UNKNOWN, "screen and transcript both unreadable"
    if f["waiting"]:
        return INACTIVE, "waiting: " + "; ".join([f["waiting"]] + tail)
    return INACTIVE, "idle; " + "; ".join(tail)


def waiting_reason(prompt, status, wind_down_reason):
    """Why the pane is waiting rather than idle, or "". Pure, for --selftest.

    Runners before and after the typed-signal change coexist, so both
    vocabularies of `windDown.reason` are read. `record_waiting` is Claude
    Code's own record of a permission prompt or a question, so it counts
    whatever the screen read. The runner's prompt check - the older build's
    `grid_busy`, the newer build's `prompt_not_ready` - decides only where this
    script's own screen read could not locate the prompt. Every other reason is
    about whether the session is working, which this report reads itself."""
    if prompt == "text":
        return "a dialog or typed text at the prompt"
    if status == "waiting_human":
        return "the session says it is waiting for you"
    if wind_down_reason == "record_waiting":
        return "Claude Code's record says it is waiting on a human (a permission prompt or a question)"
    if prompt == "unknown" and wind_down_reason == "grid_busy":
        return "the runner's screen check: not at an empty prompt"
    if prompt == "unknown" and wind_down_reason == "prompt_not_ready":
        return "the runner's prompt check: an unsent draft, a dialog, or no prompt"
    return ""


def last_record(path, chunk=1 << 20):
    """Timestamp (ms) of the newest user/assistant record in a JSONL file,
    scanning backwards from the end; None when it has none or cannot be read.
    An idle session keeps appending metadata records, so the newest real turn
    can sit megabytes from the end."""
    try:
        with open(path, "rb") as fh:
            pos = os.fstat(fh.fileno()).st_size
            carry = b""
            while pos > 0:
                step = min(chunk, pos)
                pos -= step
                fh.seek(pos)
                lines = (fh.read(step) + carry).split(b"\n")
                carry = lines.pop(0) if pos > 0 else b""  # a line cut by the chunk edge
                for line in reversed(lines):
                    if b'"timestamp"' not in line:
                        continue
                    try:
                        e = json.loads(line)
                    except ValueError:
                        continue
                    if e.get("type") in ("user", "assistant") and e.get("timestamp"):
                        return cfs.ms(e["timestamp"])
    except (OSError, ValueError):
        return None
    return None


def last_activity(row):
    """Newest user/assistant record (ms) in the session's transcript or any of
    its subagent transcripts; None when none is found."""
    cd = row.get("configDir") or os.path.expanduser("~/.claude")
    sid = row.get("claudeSessionId") or ""
    if not sid:
        return None
    paths = glob.glob(os.path.join(cd, "projects", "*", f"{sid}.jsonl"))
    for p in list(paths):
        paths += glob.glob(os.path.join(os.path.dirname(p), sid, "subagents", "*.jsonl"))
    stamps = [t for t in (last_record(p) for p in paths) if t]
    return max(stamps) if stamps else None


def account(row):
    """The account label the runner recorded. It labels the default config dir
    (~/.claude) `unknown`; that one is named `default`. A row with no config
    dir keeps the label it has."""
    label = (row.get("account") or {}).get("label")
    cd = row.get("configDir")
    if label in (None, "", "unknown") and cd and os.path.normpath(cd) == os.path.expanduser("~/.claude"):
        return "default"
    return label or os.path.basename(cd or "") or "?"


def registry_rows():
    """{terminalId: [registry row, ...]} over every state, newest first."""
    out = {}
    for r in cfs.read("/sessions/history?limit=5000", "sessions"):
        if r.get("terminalId"):
            out.setdefault(r["terminalId"], []).append(r)
    for rs in out.values():
        rs.sort(key=lambda r: (r.get("state") == "open", r.get("openedAt") or 0), reverse=True)
    return out


def pick_row(rows, live):
    """(row, note): the registry row for a pane. The session the running claude
    holds wins; else the newest open row; else the newest row of any state.
    `note` says why the choice is not certain, or is ""."""
    open_rows = [r for r in rows if r.get("state") == "open"]
    if live:
        held = [r for r in rows if r.get("claudeSessionId") in live]
        if held:
            return held[0], ""
        if rows:  # never pass an older session off as the one running here
            return (open_rows or rows)[0], (f"claude runs session {sorted(live)[0][:8]}, which the registry "
                                            f"does not hold for this tab (after /clear?)")
    if open_rows:
        note = f"{len(open_rows)} open registry rows for this tab; the newest is shown" if len(open_rows) > 1 else ""
        return open_rows[0], note
    return (rows[0] if rows else None), ""


def pane(term, rows, table, ready):
    """(facts, row) for one terminal."""
    f = {"claude": None, "unreadable": "", "other": "", "exited": "", "unbound": "", "shells": 0,
         "screen_busy": None, "waiting": "", "status": None, "note": "", "last_activity_ms": None,
         "now_ms": time.time() * 1000}
    live, claudes = set(), []
    if table is not None and term.get("pid") in table:
        claudes, shells, other = cfs.proc_facts(table, term["pid"])
        f["claude"], f["shells"] = bool(claudes), len(shells)
        if not claudes and other:
            f["other"] = " ".join(table[other[0]][1])[:60]
        live = cfs.live_session_ids(claudes)
    elif not term.get("isAlive"):
        f["claude"] = False
    elif table is None:
        f["unreadable"] = "no process table"
    else:
        f["unreadable"] = f"the pane's pid {term.get('pid')} is not in the process table"
    row, note = pick_row(rows, live)
    if f["claude"] is False:
        had = row is not None
        f["exited"] = ("exited" if had else "shell (no Claude session registered)") + \
            (", finished" if had and row.get("finished") else "")
        if not term.get("isAlive"):
            f["exited"] = "pane exited" + (" (its Claude session with it)" if had else "")
        return f, row
    if not f["claude"]:
        return f, row
    if row is None:
        f["unbound"] = "claude is running, but the runner registry has no session for this tab"
        return f, row
    if note.startswith("claude runs"):
        f["unbound"] = note
    rp = ready.get(row.get("claudeSessionId")) or {}
    coord = rp.get("sessionStatus")
    if row.get("finished") and coord not in (None, "finished"):
        f["status"] = f"finished (runner) / {coord} (coord)"
    else:
        f["status"] = "finished" if row.get("finished") else coord
    raw = cfs.raw_tail(term["id"])
    if raw is not None:
        f["screen_busy"] = cfs.running_turn(cfs.ANSI_RE.sub("", raw[-4000:]))
        # Only a positive reading counts: a prompt the replay cannot locate is
        # the reader's limit, not a session waiting on anyone.
        prompt = cfs.prompt_state(raw, cfs.wrap_cols(term))[0]
    else:
        prompt = "unknown"
    f["waiting"] = waiting_reason(prompt, rp.get("sessionStatus"), (rp.get("windDown") or {}).get("reason"))
    if not f["unbound"]:
        f["note"] = note
    f["last_activity_ms"] = last_activity(row)
    return f, row


def collect(idle_min):
    """(tabs, warnings): [(tab name, [row, ...]), ...] in the runner's tab order."""
    terms = cfs.terminals()
    rows = registry_rows()
    pages = cfs.page_names()
    table = cfs.proc_table()
    ready, ready_err = cfs.readiness()
    warnings = []
    if ready_err:
        warnings.append(f"runner readiness unreadable ({ready_err[:120]}): each session's declared "
                        "status and the runner's own screen check were not used")
    if table is None:
        warnings.append("no process table: every live pane reads UNKNOWN")
    if not pages:
        warnings.append("tab names unreadable (read only from the runner's WebKitGTK storage on Linux); "
                        "tabs are shown by page id")
    order = {pid: i for i, pid in enumerate(pages)}
    groups = {}
    for t in terms:
        f, row = pane(t, rows.get(t["id"], []), table, ready)
        state, detail = judge(f, idle_min)
        groups.setdefault(t.get("pageId"), []).append({
            "terminalId": t["id"], "state": state, "detail": detail,
            "name": (row or {}).get("resumeName") or t.get("title") or "",
            "account": account(row) if row else "-",
            "sessionId": (row or {}).get("claudeSessionId"),
            "resumeCommand": (row or {}).get("resumeCommand")})
    tabs = []
    names = [pages.get(p) for p in groups]
    for pid in sorted(groups, key=lambda p: (order.get(p, len(order)), str(p))):
        name = pages.get(pid) or f"page {str(pid)[:8]}"
        if pages.get(pid) and names.count(pages[pid]) > 1:
            name += f" [{str(pid)[:8]}]"  # two tabs can carry one name
        tabs.append((name, sorted(groups[pid], key=lambda r: (r["state"] != ACTIVE, r["name"].lower()))))
    return tabs, warnings


def print_report(tabs, warnings):
    for w in warnings:
        print("WARNING: " + w)
    total = [r for _, rs in tabs for r in rs]
    n = {s: sum(r["state"] == s for r in total) for s in (ACTIVE, INACTIVE, UNKNOWN)}
    print(f"{len(total)} open terminal(s) in {len(tabs)} tab(s): {n[ACTIVE]} active, {n[INACTIVE]} inactive"
          + (f", {n[UNKNOWN]} unknown" if n[UNKNOWN] else ""))
    w_acct = max([7] + [len(r["account"]) for r in total])
    for name, rs in tabs:
        a = sum(r["state"] == ACTIVE for r in rs)
        print(f"\n== {name}  ({len(rs)}: {a} active, {len(rs) - a} not)")
        print(f"  {'STATE':<9} {'ACCOUNT':<{w_acct}}  {'SESSION':<8}  NAME (for /resume)")
        for r in rs:
            sid = (r["sessionId"] or "-")[:8]
            print(f"  {r['state']:<9} {r['account']:<{w_acct}}  {sid:<8}  {r['name'][:70]}  - {r['detail']}")


def selftest():
    now = 10_000_000.0
    base = {"claude": True, "unreadable": "", "other": "", "exited": "exited", "unbound": "", "shells": 0,
            "screen_busy": False, "waiting": "", "status": None, "note": "", "last_activity_ms": now - 60 * 60000,
            "now_ms": now}
    cases = [
        ({}, INACTIVE),
        ({"screen_busy": True}, ACTIVE),
        ({"last_activity_ms": now - 2 * 60000}, ACTIVE),
        ({"shells": 3}, INACTIVE),                       # a watcher left running is not a turn
        ({"shells": 3, "screen_busy": True}, ACTIVE),
        ({"status": "working"}, INACTIVE),               # the work axis, not liveness
        ({"status": "finished"}, INACTIVE),
        ({"waiting": "a dialog"}, INACTIVE),
        ({"waiting": "a dialog", "screen_busy": True}, ACTIVE),
        ({"claude": False}, INACTIVE),
        ({"claude": False, "other": "python3 serve.py"}, UNKNOWN),
        ({"claude": None, "unreadable": "no process table"}, UNKNOWN),
        ({"unbound": "claude runs another session"}, UNKNOWN),
        ({"unbound": "x", "screen_busy": True}, UNKNOWN),  # the name shown would be the wrong session's
        ({"screen_busy": None, "last_activity_ms": None}, UNKNOWN),
        ({"screen_busy": None}, INACTIVE),
        ({"last_activity_ms": None}, INACTIVE),
    ]
    bad = []
    for over, want in cases:
        got, why = judge({**base, **over}, 5)
        if got != want:
            bad.append(f"{over}: want {want}, got {got} ({why})")
    live = {"b"}
    rows = [{"state": "open", "claudeSessionId": "a", "openedAt": 2}, {"state": "open", "claudeSessionId": "b",
                                                                        "openedAt": 1}]
    if pick_row(rows, live)[0]["claudeSessionId"] != "b":
        bad.append("pick_row must prefer the session the running claude holds")
    if pick_row(rows, set())[0]["claudeSessionId"] != "a" or not pick_row(rows, set())[1]:
        bad.append("pick_row with no live id must take the newest open row and say there were two")
    if not pick_row(rows, {"z"})[1].startswith("claude runs"):
        bad.append("pick_row must flag a live session the registry does not hold")
    old = [{"state": "closed", "claudeSessionId": "old", "openedAt": 1}]
    if not pick_row(old, {"new"})[1].startswith("claude runs"):
        bad.append("pick_row must flag a live session even when the tab has only closed rows")
    if "two" not in judge({**base, "note": "two open rows"}, 5)[1] or \
            "two" not in judge({**base, "note": "two open rows", "screen_busy": True}, 5)[1]:
        bad.append("a row-pick note must reach the reason, active or not")
    if pick_row([], set())[0] is not None:
        bad.append("pick_row with no rows must return None")
    waits = [  # (prompt, declared status, windDown.reason) -> waiting text contains / "" for none
        (("empty", None, None), ""),
        (("unknown", None, "grid_busy"), "screen check"),        # the older build's vocabulary
        (("empty", None, "grid_busy"), ""),                      # this script's own read wins
        (("unknown", None, "prompt_not_ready"), "prompt check"),  # the newer build's
        (("empty", None, "prompt_not_ready"), ""),
        (("empty", None, "record_waiting"), "waiting on a human"),  # a typed record counts whatever the screen
        (("unknown", None, "record_busy"), ""),
        (("unknown", None, "record_shell"), ""),
        (("unknown", None, "record_unknown"), ""),
        (("unknown", None, "transcript_age_unknown"), ""),
        (("text", None, "record_waiting"), "typed text"),
        (("empty", "waiting_human", None), "says it is waiting"),
    ]
    for args, want in waits:
        got = waiting_reason(*args)
        if (want and want not in got) or (not want and got):
            bad.append(f"waiting_reason{args}: want {want!r}, got {got!r}")
    for line in bad:
        print("SELFTEST FAIL: " + line)
    print(f"selftest: {len(cases)} state cases + 6 row-pick cases + {len(waits)} waiting cases, "
          f"{len(bad)} failure(s)")
    return 1 if bad else 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--idle-min", type=int, default=5,
                    help="transcript activity newer than this many minutes counts as active (default 5)")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--port", help="runner API port (default $QONTINUI_RUNNER_API_PORT or 9876)")
    ap.add_argument("--selftest", action="store_true", help="run the state rule table and exit; reads nothing")
    a = ap.parse_args()
    for stream in (sys.stdout, sys.stderr):  # a piped Windows console is cp1252
        try:
            stream.reconfigure(encoding="utf-8", errors="backslashreplace")
        except (AttributeError, ValueError, OSError):
            pass
    if a.selftest:
        return selftest()
    if a.port:
        cfs.API = f"http://127.0.0.1:{a.port}"
    try:
        tabs, warnings = collect(a.idle_min)
    except cfs.READ_ERRORS as e:
        print(f"UNKNOWN: could not read the runner at {cfs.API}: {e}", file=sys.stderr)
        return 3
    if a.json:
        print(json.dumps({"warnings": warnings, "tabs": [{"tab": n, "sessions": rs} for n, rs in tabs]},
                         indent=1))
    else:
        print_report(tabs, warnings)
    return 0


if __name__ == "__main__":
    sys.exit(main())
