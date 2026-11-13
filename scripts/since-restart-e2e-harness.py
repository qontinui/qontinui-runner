#!/usr/bin/env python3
"""End-to-end harness for the Since-restart roster, on a TEMP runner.

Plan `2026-11-11-since-restart-roster-counts-closed-sessions-as-missing`,
Phase 8 (and Phase 0's "Unknown 2"). It drives one temp runner, spawned by the
dev supervisor (`:9875`), through a restart that keeps its PTY holders, then
closes, a respawn under another account directory, and a drained page, and
records what the runner's ledger and the Since-restart strip say.

It NEVER touches the primary runner (`:9876`): every runner it signals is one it
spawned itself, and the PID it hard-kills is checked to be listening on the temp
runner's own port first. Linux only (it reads `/proc`).

Usage (from anywhere):

    python3 scripts/since-restart-e2e-harness.py --label fixed \\
        --worktree-path <runner checkout> --out <results dir>
    python3 scripts/since-restart-e2e-harness.py --label control \\
        --git-ref origin/main --out <results dir>
    # or reuse a runner already spawned with the stub env:
    python3 scripts/since-restart-e2e-harness.py --label fixed --runner-id test-… --out …

The session layout (15 sessions over 3 pages, 5 each):

    default : d1 d2  -> closed through the UI Bridge tab close (`explicit`) after coming back
              d3     -> closed through DELETE /terminals/{id} after coming back
              d4 d5  -> came back, left alone
    e2e-p2  : q1 q2  -> DELETE after coming back
              q3     -> bound to a second account dir, DELETEd after coming back,
                        then respawned through POST /control/sessions/resume
              q4 q5  -> came back, left alone
    e2e-p3  : r1 r2  -> their holders are killed with the runner, so they never
                        come back; the page is opened while the drain is forced
              r3-r5  -> came back, left alone (page never opened before the drain)

Expected on a fixed build: `missing` = {r1, r2}; `restoreOwed` true for exactly
those; the strip's waiting count = 2; and the same after the closed rows are
pruned. On the control (origin/main) the closed-after-return sessions are
expected to read `missing` (Reading 1's shape).

Every raw reading is appended to `<out>/<label>-<utc>.jsonl` with an RFC 3339
UTC timestamp; the summary goes to `<out>/<label>-<utc>-summary.json`.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

SUPERVISOR = "http://127.0.0.1:9875"
PRIMARY_PORT = 9876
CLOSED_PRUNE_OFFSET_MS = -25 * 3600 * 1000  # past CLOSED_RETENTION_MS (24 h)
POLL_TICK_S = 45  # LIVENESS_POLL_INTERVAL_SECS

STUB_CLAUDE = r"""#!/bin/bash
# Stub `claude` for the since-restart E2E harness: stays alive as THIS bash
# process (so /proc/<pid>/comm stays `claude`), and writes the session
# registry entry the runner's claude_session_registry reads.
sid=""
prev=""
for a in "$@"; do
  case "$prev" in --session-id|--resume|-r) sid="$a" ;; esac
  prev="$a"
done
dir="${CLAUDE_CONFIG_DIR:-}"
if [ -n "$dir" ] && [ -n "$sid" ]; then
  mkdir -p "$dir/sessions"
  printf '{"pid":%d,"sessionId":"%s","cwd":"%s","kind":"interactive","status":"idle"}\n' \
    "$$" "$sid" "$PWD" > "$dir/sessions/$$.json"
fi
echo "stub claude session=$sid"
while :; do sleep 3600; done
"""


def now_rfc3339() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ")


class Log:
    def __init__(self, path: Path):
        self.path = path
        path.parent.mkdir(parents=True, exist_ok=True)

    def rec(self, step: str, **data) -> None:
        line = {"at": now_rfc3339(), "step": step, **data}
        with self.path.open("a") as f:
            f.write(json.dumps(line, default=str) + "\n")
        brief = json.dumps(data, default=str)
        print(f"[{line['at']}] {step}: {brief[:400]}", flush=True)


def http(method: str, url: str, body=None, timeout: float = 30.0):
    """(status, parsed-json-or-text). Never raises on an HTTP error status."""
    data = None
    headers = {}
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            raw = r.read().decode("utf-8", "replace")
            status = r.status
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", "replace")
        status = e.code
    except (urllib.error.URLError, TimeoutError, ConnectionError, OSError) as e:
        return 0, f"unreachable: {e}"
    try:
        return status, json.loads(raw)
    except json.JSONDecodeError:
        return status, raw


def data_of(v):
    if isinstance(v, dict) and "data" in v and isinstance(v["data"], (dict, list)):
        return v["data"]
    return v


class Runner:
    def __init__(self, rid: str, port: int, log: Log):
        if port == PRIMARY_PORT or rid == "primary":
            raise SystemExit("refusing to drive the primary runner")
        self.id, self.port, self.log = rid, port, log
        self.base = f"http://127.0.0.1:{port}"
        self.root = Path.home() / ".config/com.qontinui.runner/instances" / rid

    def get(self, path, **kw):
        return http("GET", self.base + path, **kw)

    def post(self, path, body=None, **kw):
        return http("POST", self.base + path, body if body is not None else {}, **kw)

    def delete(self, path):
        return http("DELETE", self.base + path)

    def health(self):
        s, v = self.get("/health", timeout=10)
        return v if s == 200 else None

    def wait_healthy(self, limit_s=240):
        end = time.time() + limit_s
        while time.time() < end:
            h = self.health()
            if h:
                return h
            time.sleep(2)
        raise SystemExit(f"temp runner {self.id} not healthy after {limit_s}s")

    def supervisor_record(self):
        return supervisor_record_of(self.id)


def supervisor_record_of(rid: str):
    s, v = http("GET", f"{SUPERVISOR}/runners")
    rs = v if isinstance(v, list) else (v or {}).get("runners", []) if isinstance(v, dict) else []
    for r in rs if isinstance(rs, list) else []:
        if isinstance(r, dict) and r.get("id") == rid:
            return r
    return None


# ---------------------------------------------------------------------------
# Spawning / teardown
# ---------------------------------------------------------------------------


def spawn(args, log: Log, env: dict) -> Runner:
    body = {
        "rebuild": True,
        "async": True,
        "frontend_strict": True,
        "requester_id": f"since-restart-e2e-{args.label}",
        "extra_env": env,
    }
    if args.worktree_path:
        body["worktree_path"] = args.worktree_path
    if args.git_ref:
        body["git_ref"] = args.git_ref
    s, v = http("POST", f"{SUPERVISOR}/runners/spawn-test", body, timeout=120)
    log.rec("spawn.request", status=s, body=v)
    if s not in (200, 202) or not isinstance(v, dict):
        raise SystemExit(f"spawn refused: {s} {v}")
    rid, port = v.get("id"), v.get("port")
    sub = v.get("submission_id")
    if sub:
        end = time.time() + 3 * 3600
        while time.time() < end:
            s2, st = http("GET", f"{SUPERVISOR}/build/{sub}/status")
            state = ((st or {}).get("status") or {}).get("state") if isinstance(st, dict) else None
            if state in ("succeeded", "failed"):
                log.rec("spawn.outcome", status=s2, body=st)
                spawn_out = (st or {}).get("spawn") or {}
                if state == "failed" or spawn_out.get("http_status") not in (200, 201, None):
                    raise SystemExit(f"spawn failed: {json.dumps(st)[:2000]}")
                b = spawn_out.get("body") or {}
                rid, port = b.get("id", rid), b.get("port", port)
                break
            time.sleep(20)
        else:
            raise SystemExit("spawn did not finish in 3 h")
    return Runner(rid, int(port), log)


def env_for(stub_dir: Path, acct_a: Path) -> dict:
    return {
        "PATH": f"{stub_dir}:{os.environ.get('PATH', '/usr/bin:/bin')}",
        "CLAUDE_CONFIG_DIR": str(acct_a),
    }


def holder_sessions_for(root: Path) -> list[int]:
    """Session ids (setsid leaders) of PTY holders serving `root`'s pane dir."""
    pane_dir = str(root / "home")
    sids = set()
    for p in Path("/proc").iterdir():
        if not p.name.isdigit():
            continue
        try:
            cmd = (p / "cmdline").read_bytes().replace(b"\0", b" ").decode("utf-8", "replace")
        except OSError:
            continue
        if "pty-holder" in cmd and pane_dir in cmd:
            try:
                sids.add(os.getsid(int(p.name)))
            except OSError:
                pass
    return sorted(s for s in sids if s > 1 and s != os.getsid(0))


def teardown(r: Runner | None, log: Log, scratch: list[Path]) -> None:
    if r is not None:
        s, v = http("POST", f"{SUPERVISOR}/runners/{r.id}/stop", {}, timeout=120)
        log.rec("teardown.stop", status=s, body=v)
        for sid in holder_sessions_for(r.root):
            try:
                os.killpg(sid, signal.SIGKILL)
                log.rec("teardown.holder_killed", sid=sid)
            except OSError as e:
                log.rec("teardown.holder_kill_failed", sid=sid, error=str(e))
        time.sleep(3)
        if r.root.exists():
            shutil.rmtree(r.root, ignore_errors=True)
        log.rec("teardown.root", root=str(r.root), exists=r.root.exists(),
                record=r.supervisor_record())
    for d in scratch:
        shutil.rmtree(d, ignore_errors=True)
    log.rec("teardown.scratch_removed", dirs=[str(d) for d in scratch],
            remaining=[str(d) for d in scratch if d.exists()])


# ---------------------------------------------------------------------------
# Readings
# ---------------------------------------------------------------------------


def open_records(r: Runner) -> list[dict]:
    s, v = r.post("/ui-bridge/test/list-lifecycle-open")
    return (v or {}).get("open_records", []) if isinstance(v, dict) else []


def ledger(r: Runner) -> dict:
    s, v = r.get("/control/sessions/ledger", timeout=60)
    return data_of(v) if s == 200 else {"_status": s, "_body": v}


def element_text(r: Runner, el: str):
    s, v = r.get(f"/ui-bridge/control/element/{el}", timeout=30)
    d = data_of(v)
    if s != 200 or not isinstance(d, dict):
        return None, {"status": s, "body": v}
    st = d.get("state") if isinstance(d.get("state"), dict) else {}
    return st.get("textContent") or d.get("textContent"), d


def click(r: Runner, el: str, action: str = "click"):
    return r.post(f"/ui-bridge/control/element/{el}/action", {"action": action}, timeout=30)


def waiting_count(strip_text: str | None):
    if not strip_text:
        return None
    m = re.search(r"(\d+)(?: sessions?)? waiting", strip_text)
    return int(m.group(1)) if m else 0


def ids_in(led: dict, bucket: str) -> set[str]:
    return {str(o.get("claudeSessionId", "")).lower() for o in led.get(bucket, []) or []}


def owed_ids(led: dict) -> set[str]:
    out = set()
    for bucket in ("missing", "returned", "closedSince", "closedByUser", "finished"):
        for o in led.get(bucket, []) or []:
            if o.get("restoreOwed") is True:
                out.add(str(o.get("claudeSessionId", "")).lower())
    return out


def reading(r: Runner, log: Log, tag: str) -> dict:
    led = ledger(r)
    strip, strip_raw = element_text(r, "terminal.since-restart-strip")
    # The section renders only while the review panel is open.
    click(r, "terminal.since-restart-review")
    time.sleep(2)
    section, section_raw = element_text(r, "terminal.since-restart-section")
    click(r, "terminal.since-restart-review")
    out = {
        "ledger": led,
        "strip_text": strip,
        "section_text": section,
        "strip_raw": strip_raw if strip is None else None,
        "section_raw": section_raw if section is None else None,
    }
    log.rec(f"reading.{tag}", **out)
    return out


# ---------------------------------------------------------------------------
# The run
# ---------------------------------------------------------------------------

LAYOUT = {
    "default": ["d1", "d2", "d3", "d4", "d5"],
    "e2e-p2": ["q1", "q2", "q3", "q4", "q5"],
    "e2e-p3": ["r1", "r2", "r3", "r4", "r5"],
}
UI_CLOSE = ["d1", "d2"]
DELETE_CLOSE = ["d3", "q1", "q2"]
RESPAWN = "q3"
NEVER_BACK = ["r1", "r2"]


def encode_project_path(p: str) -> str:
    """`terminal::transcript::encode_project_path`: every non-alnum byte -> '-'."""
    p = p.rstrip("/") or p
    return "".join(c if (c.isascii() and c.isalnum()) else "-" for c in p)


def run(args) -> int:
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    out = Path(args.out)
    log = Log(out / f"{args.label}-{stamp}.jsonl")
    work = Path(args.work_dir) if args.work_dir else out / f"{args.label}-{stamp}-work"
    stub_dir, acct_a, acct_b, cwd = (work / "bin", work / "acct-a", work / "acct-b", work / "cwd")
    for d in (stub_dir, acct_a, acct_b, cwd):
        d.mkdir(parents=True, exist_ok=True)
    (stub_dir / "claude").write_text(STUB_CLAUDE)
    (stub_dir / "claude").chmod(0o755)
    scratch = [work]
    summary: dict = {"label": args.label, "started_at": now_rfc3339(), "assertions": {}}
    r: Runner | None = None
    try:
        if args.runner_id:
            recd = supervisor_record_of(args.runner_id)
            if not recd:
                raise SystemExit(f"no such runner {args.runner_id}")
            r = Runner(args.runner_id, int(recd["port"]), log)
            end = time.time() + 3 * 3600
            while not (supervisor_record_of(r.id) or {}).get("running") and time.time() < end:
                time.sleep(20)
        else:
            r = spawn(args, log, env_for(stub_dir, acct_a))
        h = r.wait_healthy()
        hd = data_of(h)
        sha = hd.get("gitSha") or (hd.get("provenance") or {}).get("gitSha")
        summary.update(runner_id=r.id, port=r.port, git_sha=sha, build_id=hd.get("buildId"))
        log.rec("runner.health", git_sha=sha, build_id=hd.get("buildId"), root=str(r.root))
        s, v = r.post("/ui-bridge/test/force-coord-drain", {"action": "release"})
        have_fixture = s == 200
        summary["drain_fixture"] = have_fixture
        log.rec("drain.fixture_probe", status=s, body=v)

        # Wait for a drain read that allows creates.
        end = time.time() + 300
        while time.time() < end:
            s, v = r.post("/terminals", {"title": "probe", "workingDir": str(cwd)})
            if s in (200, 201):
                tid = (data_of(v) or {}).get("id") or (data_of(v) or {}).get("terminalId")
                if tid:
                    r.delete(f"/terminals/{tid}")
                break
            log.rec("create.probe_refused", status=s, body=v)
            time.sleep(15)
        else:
            raise SystemExit("terminal creates never allowed (drain state)")
        # The probe pane left a row; start from an empty store.
        r.post("/ui-bridge/test/clear-lifecycle-store")

        # 1. Seed sessions: real panes running the stub through the identity shim.
        names: dict[str, dict] = {}
        for page, keys in LAYOUT.items():
            for k in keys:
                body = {"title": f"e2e-{k}", "workingDir": str(cwd), "pageId": page,
                        "initialCommand": "claude"}
                s, v = r.post("/terminals", body)
                d = data_of(v) or {}
                tid = d.get("id") or d.get("terminalId") or (d.get("terminal") or {}).get("id")
                names[k] = {"terminal_id": tid, "page": page}
                log.rec("create", key=k, status=s, terminal_id=tid)
                if not tid:
                    raise SystemExit(f"create {k} failed: {s} {v}")
                time.sleep(0.5)
        # Wait for every pane's row to be confirmed by the shim.
        by_tid = {}
        end = time.time() + 120
        while time.time() < end:
            recs = open_records(r)
            by_tid = {x.get("terminal_id"): x for x in recs}
            if all(by_tid.get(n["terminal_id"], {}).get("confirmed") for n in names.values()):
                break
            time.sleep(3)
        for k, n in names.items():
            row = by_tid.get(n["terminal_id"]) or {}
            n["session_id"] = (row.get("session_id") or "").lower()
            n["confirmed"] = row.get("confirmed")
        log.rec("seeded", sessions=names)
        if not all(n["session_id"] for n in names.values()):
            raise SystemExit("not every pane got a lifecycle row")

        # q3 moves to the second account dir, with a transcript the resume door needs.
        q3 = names[RESPAWN]
        s, v = r.post("/control/session-open", {
            "terminal_id": q3["terminal_id"], "session_id": q3["session_id"],
            "source": "startup", "provider": "claude", "config_dir": str(acct_b), "cwd": str(cwd)})
        tdir = acct_b / "projects" / encode_project_path(str(cwd))
        tdir.mkdir(parents=True, exist_ok=True)
        (tdir / f"{q3['session_id']}.jsonl").write_text(json.dumps({
            "type": "user", "sessionId": q3["session_id"], "cwd": str(cwd),
            "message": {"role": "user", "content": "e2e"}}) + "\n")
        log.rec("q3.account_b", status=s, body=v, transcript=str(tdir))

        # Let a poll tick see every pane, then capture the restore set.
        time.sleep(POLL_TICK_S + 5)
        s, v = r.post("/control/sessions/ledger/capture?reason=e2e-pre-restart")
        log.rec("ledger.capture", status=s, persisted=(data_of(v) or {}).get("persisted"))

        # 2. Restart keeping holders: hard-kill the TEMP runner's own pid.
        rec = r.supervisor_record() or {}
        pid = rec.get("pid")
        ss = subprocess.run(["ss", "-ltnpH", f"sport = :{r.port}"], capture_output=True, text=True).stdout
        if not pid or f"pid={pid}," not in ss:
            raise SystemExit(f"refusing to kill: pid {pid} is not the listener on :{r.port} ({ss!r})")
        # The supervisor relaunches a temp runner from the FRESHEST slot exe, not
        # from the exe it spawned, so a concurrent build elsewhere would swap the
        # binary under test. Refuse before the kill when the pick has moved.
        sb, builds = http("GET", f"{SUPERVISOR}/builds")
        slot_now = (builds or {}).get("last_successful_slot") if isinstance(builds, dict) else None
        exe = (rec.get("source_exe") or {}) if isinstance(rec.get("source_exe"), dict) else {}
        log.rec("restart.exe_pick", runner_slot=exe.get("slot_id"), runner_sha=exe.get("build_sha"),
                freshest_slot=slot_now)
        if exe.get("slot_id") is None or slot_now != exe.get("slot_id"):
            raise SystemExit(f"refusing to restart: the relaunch would run slot {slot_now}, "
                             f"not this runner's slot {exe.get('slot_id')}")
        stub_pids = {k: stub_pid_for(n["session_id"]) for k, n in names.items()}
        os.kill(int(pid), signal.SIGKILL)
        log.rec("restart.killed_runner", pid=pid)
        # The never-back sessions lose their holders with the runner.
        for k in NEVER_BACK:
            sp = stub_pids.get(k)
            if sp:
                sid = os.getsid(sp)
                if sid > 1 and sid != os.getsid(0):
                    os.killpg(sid, signal.SIGKILL)
                    log.rec("restart.killed_holder", key=k, stub_pid=sp, holder_sid=sid)
        time.sleep(2)
        # wait=false: the supervisor's own 30 s health wait is shorter than a
        # temp runner's boot with fifteen holders to adopt; poll it here.
        s, v = http("POST", f"{SUPERVISOR}/runners/{r.id}/start?wait=false", {}, timeout=120)
        log.rec("restart.start", status=s, body=v)
        if s not in (200, 202):
            raise SystemExit(f"relaunch refused: {s} {v}")
        h2 = data_of(r.wait_healthy(limit_s=600))
        sha2 = h2.get("gitSha") or (h2.get("provenance") or {}).get("gitSha")
        log.rec("restart.relaunched_build", git_sha=sha2, build_id=h2.get("buildId"))
        if sha2 != sha:
            raise SystemExit(f"the relaunch ran {sha2}, not the build under test {sha}")
        survived = {k: (p is not None and Path(f"/proc/{p}").exists()) for k, p in stub_pids.items()}
        log.rec("restart.holders_survived", survived=survived, stub_pids=stub_pids)

        # Adoption and two poll ticks, so "came back" is observed.
        end = time.time() + 180
        adopted = set()
        while time.time() < end:
            s, v = r.get("/terminals")
            lst = data_of(v)
            lst = lst.get("terminals", lst) if isinstance(lst, dict) else lst
            adopted = {t.get("id") for t in (lst or []) if isinstance(t, dict)}
            want = {n["terminal_id"] for k, n in names.items() if k not in NEVER_BACK}
            if want <= adopted:
                break
            time.sleep(5)
        summary["adopted"] = {k: n["terminal_id"] in adopted for k, n in names.items()}
        log.rec("restart.adopted", adopted=summary["adopted"])
        time.sleep(2 * POLL_TICK_S + 10)
        summary["reading_after_restart"] = reading(r, log, "after_restart")

        # 3. Closes. The tab close button exists only for panes on the ACTIVE
        # page, which is whichever page the webview had open before the
        # restart, so the UI-closed pair is picked from the panes that have one.
        ui_close, delete_close = pick_closes(r, names, log)
        summary["closes"] = {"ui": ui_close, "delete": delete_close}
        for k in ui_close:
            # The close button is a hover action (pointer-events:none until the
            # zone is hovered), so a plain click is refused as disabled.
            s, v = click(r, f"terminal.close-session-{names[k]['terminal_id']}", "hoverClick")
            log.rec("close.ui", key=k, status=s, body=v)
            time.sleep(2)
        for k in delete_close + [RESPAWN]:
            s, v = r.delete(f"/terminals/{names[k]['terminal_id']}")
            log.rec("close.delete", key=k, status=s, body=v)
        time.sleep(POLL_TICK_S + 10)

        # 4. Respawn q3 under account B through the resume door.
        s, v = r.post("/control/sessions/resume", {"ids": [q3["session_id"]]}, timeout=90)
        log.rec("respawn.resume_door", status=s, body=v)
        summary["respawn"] = {"status": s, "body": v}

        # 5. Force the drain, then open the page whose restore has not run.
        if have_fixture:
            s, v = r.post("/ui-bridge/test/force-coord-drain",
                          {"action": "drain", "reason": "since-restart e2e"})
            log.rec("drain.forced", status=s, body=v)
            opened = open_page(r, "e2e-p3", log)
            summary["p3_opened"] = opened
        else:
            log.rec("drain.skipped", why="this build has no force-coord-drain fixture; "
                    "the device drain is the fleet's, never forced from a test")
        time.sleep(2 * POLL_TICK_S + 10)

        # 6. Read.
        first = reading(r, log, "final")
        summary["reading_final"] = first

        snapshot_instance(r, out / f"{args.label}-{stamp}-instance" / "before-prune", log)
        # Prune: re-seed with every closed row's closedAt past retention, then a tick.
        pruned = prune_closed(r, log)
        summary["prune"] = pruned
        time.sleep(2 * POLL_TICK_S + 10)
        second = reading(r, log, "after_prune")
        summary["reading_after_prune"] = second
        summary["store_after_prune"] = store_closed_ids(r)
        snapshot_instance(r, out / f"{args.label}-{stamp}-instance" / "after-prune", log)

        evaluate(summary, names, first, second, have_fixture)
        return 0
    finally:
        summary["finished_at"] = now_rfc3339()
        if not args.keep:
            teardown(r, log, scratch)
            summary["torn_down"] = True
        (out / f"{args.label}-{stamp}-summary.json").write_text(
            json.dumps(summary, indent=2, default=str))
        print(f"summary: {out / f'{args.label}-{stamp}-summary.json'}")


def stub_pid_for(session_id: str):
    for p in Path("/proc").iterdir():
        if not p.name.isdigit():
            continue
        try:
            comm = (p / "comm").read_text().strip()
            if comm != "claude":
                continue
            cmd = (p / "cmdline").read_bytes().split(b"\0")
        except OSError:
            continue
        if session_id.encode() in cmd:
            return int(p.name)
    return None


def element_ids(r: Runner) -> list[str]:
    r.post("/ui-bridge/control/discover", {})
    s, v = r.get("/ui-bridge/control/elements", timeout=30)
    els = data_of(v)
    els = els.get("elements", els) if isinstance(els, dict) else els
    return [e.get("id", "") for e in els or [] if isinstance(e, dict)]


def pick_closes(r: Runner, names: dict, log: Log):
    """Two UI closes and two DELETEs, all from panes on the ACTIVE page.

    Only a mounted pane's exit is recorded as a close (the webview's
    `handleExit`); a pane DELETEd on a page whose restore has not run leaves its
    row open and held for that restore (measured 2026-11-13, run 4), so a close
    there is no close at all."""
    pool = [k for k in names if k not in NEVER_BACK and k != RESPAWN]
    ids = set(element_ids(r))
    with_button = [k for k in pool if f"terminal.close-session-{names[k]['terminal_id']}" in ids]
    ui = with_button[:2]
    delete = with_button[2:4]
    log.rec("close.plan", with_button=with_button, ui=ui, delete=delete)
    return ui, delete


def open_page(r: Runner, page_id: str, log: Log) -> bool:
    """Click the page's tab. Tabs are named "Page N", not by page id, and their
    UI Bridge ids end in the page's index in GET /terminal-pages."""
    s, v = r.get("/terminal-pages")
    pages = [p.get("pageId") for p in (data_of(v) or {}).get("pages", [])]
    idx = pages.index(page_id) if page_id in pages else None
    cands = [i for i in element_ids(r)
             if idx is not None and i.startswith("tab-switch-to-") and i.endswith(f"-{idx}")]
    log.rec("page.candidates", page=page_id, pages=pages, candidates=cands)
    for c in cands:
        s, v = click(r, c)
        log.rec("page.click", element=c, status=s, body=v)
        if s == 200:
            return True
    return False


def snapshot_instance(r: Runner, dest: Path, log: Log) -> None:
    """Copy the ledger, aftermath and store files out of the instance root."""
    src = r.root / "home/runner"
    dest.mkdir(parents=True, exist_ok=True)
    copied = []
    for f in sorted(src.glob("session-ledger*.json")) + [src / "terminal-sessions.json"]:
        if f.is_file():
            shutil.copy2(f, dest / f.name)
            copied.append(f.name)
    log.rec("instance.snapshot", dest=str(dest), files=copied)


def later_closed_ids(led: dict) -> set[str]:
    return {str(o.get("claudeSessionId", "")).lower()
            for o in led.get("returned", []) or [] if o.get("laterClosed")}


def store_records(r: Runner) -> list[dict]:
    p = r.root / "home/runner/terminal-sessions.json"
    try:
        doc = json.loads(p.read_text())
    except (OSError, json.JSONDecodeError):
        return []
    recs = doc.get("records", doc) if isinstance(doc, dict) else doc
    if isinstance(recs, dict):
        recs = list(recs.values())
    return [x for x in recs or [] if isinstance(x, dict)]


def store_closed_ids(r: Runner) -> list[str]:
    return sorted(str(x.get("claudeSessionId") or x.get("sessionId") or "").lower()
                  for x in store_records(r) if x.get("state") == "closed")


def prune_closed(r: Runner, log: Log) -> dict:
    """Re-seed the store with each closed row's closedAt past CLOSED_RETENTION_MS."""
    now = int(time.time() * 1000)
    recs = store_records(r)
    seed = []
    for x in recs:
        sid = x.get("claudeSessionId") or x.get("sessionId")
        if not sid:
            continue
        last = x.get("lastSeenAt") or x.get("lastSeenAtMs") or now
        closed = x.get("state") == "closed"
        seed.append({k: v for k, v in {
            "sessionId": sid,
            "state": x.get("state", "open"),
            "lastSeenOffsetMs": (CLOSED_PRUNE_OFFSET_MS if closed else int(last) - now),
            "closedAtOffsetMs": CLOSED_PRUNE_OFFSET_MS if closed else None,
            "closeReason": x.get("closeReason"),
            "pageId": x.get("pageId"),
            "zoneIndex": x.get("zoneIndex"),
            "title": x.get("title"),
            "workingDir": x.get("workingDir"),
            "confirmedAt": (int(x["confirmedAt"]) - now) if x.get("confirmedAt") else None,
            "origin": x.get("origin"),
            "terminalId": x.get("terminalId"),
            "configDir": x.get("configDir"),
        }.items() if v is not None})
    before = store_closed_ids(r)
    s, v = r.post("/ui-bridge/test/seed-lifecycle-store", {"records": seed})
    out = {"status": s, "body": v, "closed_before": before, "seeded": len(seed)}
    log.rec("prune.seeded", **out)
    return out


def evaluate(summary, names, first, second, have_fixture) -> None:
    by_key = {k: n["session_id"] for k, n in names.items()}
    never_back = {by_key[k] for k in NEVER_BACK}
    led = first["ledger"]
    missing = ids_in(led, "missing")
    owed = owed_ids(led)
    a = summary["assertions"]

    def put(name, ok, **obs):
        a[name] = {"result": "PASS" if ok else "FAIL", **obs}

    put("missing_holds_only_rows_not_observed_back", missing == never_back,
        missing=sorted(missing), expected=sorted(never_back),
        closed_after_return_in_missing=sorted(missing - never_back))
    if have_fixture:
        put("restore_owed_only_for_drain_deferred_never_back", owed == never_back,
            restore_owed=sorted(owed), expected=sorted(never_back))
    else:
        a["restore_owed_only_for_drain_deferred_never_back"] = {
            "result": "SKIPPED", "why": "no drain fixture on this build",
            "restore_owed": sorted(owed)}
    wc = waiting_count(first["strip_text"])
    put("strip_waiting_equals_owed", wc == len(owed), strip_waiting=wc, owed=len(owed),
        strip_text=first["strip_text"])
    l2 = second["ledger"]
    same = all(ids_in(led, b) == ids_in(l2, b)
               for b in ("missing", "returned", "closedSince", "finished")) and owed_ids(l2) == owed
    lc1, lc2 = later_closed_ids(led), later_closed_ids(l2)
    put("reread_after_prune_unchanged", same and not summary.get("store_after_prune"),
        closed_left_in_store=summary.get("store_after_prune"),
        missing_after=sorted(ids_in(l2, "missing")), owed_after=sorted(owed_ids(l2)),
        strip_after=second["strip_text"])
    # Not one of Phase 8's four assertions, but the D4 detail the aftermath
    # record exists to keep after the registry prunes the row.
    # Informational: the prune is forced by re-seeding closedAt 25 h back, and
    # the aftermath record mirrors a row's close while the row exists, so it
    # takes that pre-boot closedAt before the prune removes the row; a real
    # prune 24 h after a real close keeps the real closedAt.
    a["later_closed_across_forced_prune"] = {
        "result": "OBSERVED", "later_closed_before": sorted(lc1),
        "later_closed_after": sorted(lc2)}
    strip = first["strip_text"] or ""
    a["strip_form"] = {"result": "OBSERVED", "deferred_form": strip.startswith("Restore deferred by drain"),
                       "strip_text": strip, "section_text": (first["section_text"] or "")[:800]}
    summary["sessions"] = by_key


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--label", required=True)
    ap.add_argument("--out", required=True)
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument("--worktree-path")
    g.add_argument("--git-ref")
    g.add_argument("--runner-id", help="an already-spawned temp runner (its env must carry the stub PATH)")
    ap.add_argument("--keep", action="store_true", help="do not stop the runner at the end")
    ap.add_argument("--work-dir", help="reuse a work dir (stub bin, account dirs) whose bin/ the "
                    "runner's PATH already names; needed with --runner-id")
    args = ap.parse_args()
    if sys.platform != "linux":
        raise SystemExit("Linux only")
    return run(args)


if __name__ == "__main__":
    sys.exit(main())
