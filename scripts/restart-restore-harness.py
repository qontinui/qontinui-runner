#!/usr/bin/env python3
"""Unplanned-restart restore harness: seed open sessions on an EPHEMERAL runner,
force-exit it, boot it again with no page interaction, and record what the
restore did.

Plan 2026-11-04-unplanned-restart-restore-is-page-gated-and-counts-unverified-resumes,
Phase 0 (ii) and the thread census. Phase 7 re-runs it against the fixed build,
with this run on origin/main as the control. It asserts nothing: it RECORDS
(summary.json in --out), so the same script describes the defect and the fix.

Usage (Linux, a built runner with `qontinui-pty-holder` beside it):

    python3 scripts/restart-restore-harness.py \
        --runner-bin <dir>/qontinui-runner --port 9891 --n 12 --pages 3 \
        --display :99 --xauthority <file> --out <dir> [--pty-holder off|on]
        [--kill-holders] [--restore-window 120] [--settle-minutes 10] [--keep]

What it does, in order:
  1. Builds a throwaway "machine": HOME, the XDG dirs, QONTINUI_INSTANCE_ROOT
     and every root-confined dir under one temp dir (default under
     $XDG_RUNTIME_DIR: holder sockets cap the path at 107 bytes). The runner is
     launched under an allowlisted environment, so no harness credential, coord
     JWT or database URL reaches it, and it is a secondary
     (QONTINUI_INSTANCE_NAME) on its own port. One rostered, non-default
     account dir gives every row a config_dir.
  2. Puts a stub `claude` first on PATH. It names itself `claude` in
     /proc/<pid>/comm (what the runner's process read matches), records argv,
     CLAUDE_CONFIG_DIR, cwd and its terminal id to stub.log, writes a transcript
     where Claude Code would, runs the SessionStart hooks of each `--settings`
     file (the runner's hook confirms the row), and stays alive until its pty
     closes.
  3. Creates --pages row pages plus one empty page through the UI Bridge (the +
     button), so the pages exist in the webview's roster; the empty page is the
     one on screen at the kill.
  4. Seeds N sessions through the runner's own door: one `POST /terminals` per
     session, spread over the row pages, each typing `claude --resume <uuid>` as
     its initial command. The runner's typed-resume sniff records each
     lifecycle row; nothing is written into the store by hand.
  5. Reads /health threadCensus at 0, N/2 and N panes (waiting out its 30 s memo).
  6. SIGKILLs the runner by ITS pid (and, with --kill-holders, the pty-holder
     pids whose env names its root: what a systemd KillMode=control-group stop
     does), then boots the same root again and touches no page.
  7. After --restore-window seconds reads GET /control/sessions/ledger,
     /restore-health, /restore-census, /terminals, the UI Bridge view, then
     POST /control/sessions/resume by the seeded ids and with {"all":true}, and
     stub.log.
  8. Placement probe (D5): on the page on screen, a keeper pane and a probe
     pane, plus a probe pane on a page not on screen; both probes are closed by
     SIGKILLing their pane shell, resumed through the door, and the zone headers,
     page roster, focused elements and recorded zones are read before and after.
  9. With --settle-minutes, reads the thread census again that long after boot.
  10. Stops the instance and its private PostgreSQL by pid and removes the temp
     root, unless --keep.

It never touches :9876, the supervisor, or any process it did not start.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path

STUB_CLAUDE = r'''#!/usr/bin/env python3
# Stub `claude` for restart-restore-harness.py: record, write a transcript, stay alive.
import fcntl, json, os, re, subprocess, sys, time, uuid
LOG = @@LOG@@
# The runner finds claude by /proc/<pid>/comm == "claude" (process_tree::is_claude_image);
# an interpreter-run stub would otherwise read as "python3" and never verify.
with open("/proc/self/comm", "w") as _c:
    _c.write("claude")
args = sys.argv[1:]
sid = None
for i, a in enumerate(args):
    if a in ("--resume", "--session-id") and i + 1 < len(args):
        sid = args[i + 1]
    elif a.startswith("--resume=") or a.startswith("--session-id="):
        sid = a.split("=", 1)[1]
cfg_env = os.environ.get("CLAUDE_CONFIG_DIR")
cfg = cfg_env or os.path.join(os.path.expanduser("~"), ".claude")
cwd = os.getcwd()
sid = sid or os.environ.get("QONTINUI_PINNED_SESSION_ID") or str(uuid.uuid4())
tdir = os.path.join(cfg, "projects", re.sub(r"[^A-Za-z0-9]", "-", cwd))
os.makedirs(tdir, exist_ok=True)
tpath = os.path.join(tdir, sid + ".jsonl")
now = time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime())
with open(tpath, "a") as f:
    f.write(json.dumps({"type": "user", "sessionId": sid, "cwd": cwd, "timestamp": now,
                        "uuid": str(uuid.uuid4()),
                        "message": {"role": "user", "content": "restart-restore-harness stub"}}) + "\n")
rec = {"ts": time.time(), "pid": os.getpid(), "ppid": os.getppid(), "argv": sys.argv,
       "CLAUDE_CONFIG_DIR": cfg_env, "cwd": cwd, "session_id": sid, "transcript": tpath,
       "terminal_id": os.environ.get("QONTINUI_TERMINAL_ID"),
       "pinned_session_id": os.environ.get("QONTINUI_PINNED_SESSION_ID")}
# Run the SessionStart hooks of every `--settings` file, as Claude Code does:
# the runner's own hook is what CONFIRMS the row (POST /control/session-open).
source = "resume" if any(a == "--resume" or a.startswith("--resume=") for a in args) else "startup"
hook_in = json.dumps({"session_id": sid, "transcript_path": tpath, "cwd": cwd,
                      "hook_event_name": "SessionStart", "source": source})
hooks_run = []
for i, a in enumerate(args):
    if a == "--settings" and i + 1 < len(args):
        try:
            settings = json.load(open(args[i + 1]))
        except (OSError, ValueError) as e:
            hooks_run.append({"settings": args[i + 1], "error": str(e)})
            continue
        for group in settings.get("hooks", {}).get("SessionStart", []):
            for hk in group.get("hooks", []):
                if hk.get("type") != "command":
                    continue
                try:
                    p = subprocess.run(["bash", "-c", hk["command"]], input=hook_in, text=True,
                                       capture_output=True, timeout=30)
                    hooks_run.append({"command": hk["command"][-60:], "rc": p.returncode})
                except Exception as e:
                    hooks_run.append({"command": hk["command"][-60:], "error": str(e)})
rec["session_start_hooks"] = hooks_run
with open(LOG, "a") as f:
    fcntl.flock(f, fcntl.LOCK_EX)
    f.write(json.dumps(rec) + "\n")
print("stub claude " + sid + " ready", flush=True)
try:
    for line in sys.stdin:
        print("stub claude got: " + line.rstrip(), flush=True)
except Exception:
    pass
'''

ALLOWED_ENV = ("USER", "LOGNAME", "LANG", "LC_ALL", "TERM", "XDG_RUNTIME_DIR",
               "DBUS_SESSION_BUS_ADDRESS", "TZ")


def log(msg: str) -> None:
    print(f"[harness {time.strftime('%H:%M:%S')}] {msg}", flush=True)


def http(method: str, url: str, body: object | None = None, timeout: float = 30.0):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method,
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            raw = r.read()
            status = r.status
    except urllib.error.HTTPError as e:
        raw, status = e.read(), e.code
    except Exception as e:  # unreachable, timeout: UNKNOWN, recorded as such
        return None, {"_error": f"{type(e).__name__}: {e}"}
    try:
        return status, json.loads(raw)
    except ValueError:
        return status, {"_raw": raw.decode(errors="replace")[:4000]}


def data_of(payload):
    if isinstance(payload, dict) and "data" in payload:
        return payload["data"]
    return payload


class Instance:
    def __init__(self, a: argparse.Namespace, base: Path):
        self.a = a
        self.base = base
        self.root = base / "root"
        self.home = base / "machine-home"
        self.stub_dir = base / "stub-bin"
        self.work = base / "work"
        self.out = Path(a.out)
        self.stub_log = self.out / "stub.log"
        self.url = f"http://127.0.0.1:{a.port}"
        # A non-default account dir, rostered, so every row carries a config_dir.
        self.account_dir = self.home / ".claude-harness"
        self.proc: subprocess.Popen | None = None
        self.boots = 0

    def prepare(self) -> None:
        for d in (self.root, self.home, self.stub_dir, self.work, self.out,
                  self.home / ".config", self.home / ".local/share", self.home / ".cache",
                  self.home / ".local/state", self.account_dir):
            d.mkdir(parents=True, exist_ok=True)
        stub = self.stub_dir / "claude"
        stub.write_text(STUB_CLAUDE.replace("@@LOG@@", repr(str(self.stub_log))))
        stub.chmod(0o755)
        self.stub_log.touch()
        # A credentials file with a far expiry and no refresh token: the
        # default account reads as logged in, and nothing tries to refresh it.
        creds = {"claudeAiOauth": {"accessToken": "harness-not-a-token",
                                   "expiresAt": int(time.time() * 1000) + 30 * 86400_000,
                                   "scopes": ["user:inference"]}}
        (self.account_dir / ".credentials.json").write_text(json.dumps(creds))
        # A real account root already has projects/; the ledger's transcript
        # index is built once at boot, so a root without it reads every
        # transcript as absent for that whole boot.
        (self.account_dir / "projects").mkdir(exist_ok=True)
        cfg = self.root / "config"
        cfg.mkdir(parents=True, exist_ok=True)
        # `claude_config_dirs` seeds the machine-global roster on first load
        # (claude_accounts::migrate_seed), so `account` on POST /terminals resolves.
        (cfg / "settings.json").write_text(json.dumps(
            {"terminal": {"pty_holder": self.a.pty_holder == "on"},
             "claude_config_dirs": [str(self.account_dir)]}, indent=2))

    def env(self) -> dict:
        env = {k: os.environ[k] for k in ALLOWED_ENV if k in os.environ}
        env.update({
            "HOME": str(self.home),
            "SHELL": "/bin/bash",
            "PATH": f"{self.stub_dir}:/usr/local/bin:/usr/bin:/bin",
            "XDG_CONFIG_HOME": str(self.home / ".config"),
            "XDG_DATA_HOME": str(self.home / ".local/share"),
            "XDG_CACHE_HOME": str(self.home / ".cache"),
            "XDG_STATE_HOME": str(self.home / ".local/state"),
            "DISPLAY": self.a.display,
            "QONTINUI_PORT": str(self.a.port),
            "QONTINUI_INSTANCE_NAME": self.a.instance_name,
            "QONTINUI_INSTANCE_ROOT": str(self.root),
            "QONTINUI_CONFIG_DIR": str(self.root / "config"),
            "QONTINUI_SECURE_STORAGE_DIR": str(self.root / "secure"),
            "QONTINUI_HOME": str(self.root / "home"),
            "QONTINUI_RUNNER_LOG_DIR": str(self.root / "logs"),
            "QONTINUI_PANIC_LOG_DIR": str(self.root / "logs"),
            "QONTINUI_EMBEDDED_PG_DIR": str(self.root / "embedded-pg"),
            "WEBVIEW2_USER_DATA_FOLDER": str(self.root / "webview"),
            "QONTINUI_DISABLE_KEYCHAIN": "1",
            "QONTINUI_SETUP_WIZARD_BYPASS": "1",
        })
        if self.a.xauthority:
            env["XAUTHORITY"] = self.a.xauthority
        return env

    def boot(self, label: str) -> dict:
        self.boots += 1
        out = open(self.out / f"runner-{label}.log", "ab")
        self.proc = subprocess.Popen([self.a.runner_bin], env=self.env(), cwd=str(self.base),
                                     stdout=out, stderr=subprocess.STDOUT,
                                     start_new_session=True)
        (self.out / f"pid-{label}").write_text(str(self.proc.pid))
        log(f"boot {label}: pid {self.proc.pid}")
        t0 = time.time()
        while time.time() - t0 < self.a.boot_timeout:
            if self.proc.poll() is not None:
                raise SystemExit(f"runner exited during boot {label} with {self.proc.returncode}; "
                                 f"see {self.out}/runner-{label}.log")
            status, body = http("GET", f"{self.url}/health", timeout=10)
            if status == 200:
                h = data_of(body)
                log(f"boot {label}: /health 200 after {time.time() - t0:.0f}s")
                return {"pid": self.proc.pid, "boot_secs": round(time.time() - t0, 1),
                        "buildId": h.get("buildId"), "gitSha": h.get("gitSha"),
                        "buildProfile": h.get("buildProfile")}
            time.sleep(2)
        raise SystemExit(f"no /health 200 within {self.a.boot_timeout}s on boot {label}")

    def holder_pids(self) -> list[int]:
        """pty-holder pids whose cmdline names THIS instance root (ours only)."""
        pids = []
        for p in Path("/proc").iterdir():
            if not p.name.isdigit():
                continue
            try:
                cmd = (p / "cmdline").read_bytes().replace(b"\0", b" ").decode(errors="replace")
                env = (p / "environ").read_bytes().decode(errors="replace")
            except OSError:
                continue
            if "qontinui-pty-holder" in cmd and str(self.root) in (cmd + env):
                pids.append(int(p.name))
        return pids

    def sigkill(self, kill_holders: bool) -> dict:
        assert self.proc is not None
        pid = self.proc.pid
        holders = self.holder_pids()
        os.kill(pid, signal.SIGKILL)
        self.proc.wait(timeout=30)
        killed = []
        if kill_holders:
            for h in holders:
                try:
                    os.kill(h, signal.SIGKILL)
                    killed.append(h)
                except ProcessLookupError:
                    pass
        time.sleep(3)
        return {"runner_pid": pid, "holders_seen": holders, "holders_killed": killed,
                "holders_alive_after": self.holder_pids()}

    def embedded_pg_pid(self) -> int | None:
        """The private cluster's postmaster, only when its cmdline names our root."""
        pidfile = self.root / "embedded-pg" / "pg-data" / "postmaster.pid"
        try:
            pid = int(pidfile.read_text().splitlines()[0])
            cmd = Path(f"/proc/{pid}/cmdline").read_bytes().decode(errors="replace")
        except (OSError, ValueError, IndexError):
            return None
        return pid if str(self.root) in cmd else None

    def stop(self) -> None:
        if self.proc and self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(timeout=30)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=30)
        for h in self.holder_pids():
            try:
                os.kill(h, signal.SIGKILL)
            except ProcessLookupError:
                pass
        pg = self.embedded_pg_pid()
        if pg:
            os.kill(pg, signal.SIGINT)  # PostgreSQL "fast" shutdown
            for _ in range(30):
                if not Path(f"/proc/{pg}").exists():
                    break
                time.sleep(1)


HOLDER_FAMILIES = ("pty-holder-ctl", "pty-holder-pum")  # comm is cut at 15 chars
SESSION_FAMILIES = ("terminal-reader", "terminal-waiter")


def thread_census(inst: Instance, label: str, panes: int, fresh_after: float | None = None) -> dict:
    """/health threadCensus, waiting for a sample taken after `fresh_after`
    (the census is memoized for 30 s, so an immediate read can predate the panes)."""
    fresh_after = time.time() if fresh_after is None else fresh_after
    t0 = time.time()
    while True:
        status, body = http("GET", f"{inst.url}/health", timeout=30)
        h = data_of(body) if status == 200 else {}
        census = h.get("threadCensus") or {}
        sampled = census.get("sampledAt")
        try:
            from datetime import datetime
            sampled_ts = datetime.fromisoformat(sampled).timestamp() if sampled else 0.0
        except ValueError:
            sampled_ts = 0.0
        if sampled_ts >= fresh_after or time.time() - t0 > 75:
            break
        time.sleep(3)
    by_name = {r.get("name"): r.get("count") for r in census.get("byName", [])}
    fam = {f: sum(c for n, c in by_name.items() if n and n.startswith(f))
           for f in SESSION_FAMILIES + HOLDER_FAMILIES}
    out = {"label": label, "panes": panes, "status": status, "sampledAt": census.get("sampledAt"),
           "fresh": sampled_ts >= fresh_after,
           "uptimeSeconds": h.get("uptimeSeconds"), "total": census.get("total"),
           "sessionThreads": census.get("sessionThreads"), "families": fam,
           "byName": by_name, "threadCeilings": h.get("threadCeilings"),
           "spawnAdmissionThreads": (h.get("spawnAdmission") or {}).get("threads")}
    log(f"census {label}: panes={panes} total={out['total']} families={fam}")
    return out


def stub_records(inst: Instance) -> list[dict]:
    out = []
    for line in inst.stub_log.read_text().splitlines():
        try:
            out.append(json.loads(line))
        except ValueError:
            pass
    return out


def restore_rows(inst: Instance) -> list[dict]:
    status, body = http("GET", f"{inst.url}/control/sessions/restore-health", timeout=30)
    return (data_of(body) or {}).get("sessions", []) if status == 200 else []


def wait_rows(inst: Instance, ids: set[str], timeout: float, confirmed: bool = True) -> list[dict]:
    t0 = time.time()
    rows: list[dict] = []
    while time.time() - t0 < timeout:
        rows = restore_rows(inst)
        ok = {r.get("claudeSessionId") for r in rows if r.get("confirmed") or not confirmed}
        if ids <= ok:
            return rows
        time.sleep(2)
    return rows


def ui(inst: Instance, method: str, path: str, body=None):
    return http(method, f"{inst.url}/ui-bridge/control{path}", body, timeout=30)


def ui_click(inst: Instance, element_id: str):
    return ui(inst, "POST", f"/element/{element_id}/action", {"action": "click"})


def ui_action(inst: Instance, component: str, action: str, params: dict | None = None):
    st, body = ui(inst, "POST", f"/component/{component}/action/{action}", params or {})
    return (data_of(body) or {}).get("result") if st == 200 else {"_status": st, "_body": body}


def ui_view(inst: Instance) -> dict:
    """Which tab/page is on screen, the page roster, and the terminal elements."""
    st, snap = ui(inst, "GET", "/snapshot")
    snap = data_of(snap) or {}
    els, zones, focused = [], [], []
    for e in snap.get("elements", []):
        eid = e.get("id") or ""
        state = e.get("state") or {}
        if state.get("focused"):
            focused.append(eid)
        if eid.startswith("terminal-zone-header-"):
            ds = state.get("dataset") or {}
            zones.append({"zoneIndex": ds.get("zoneIndex"), "claudeSessionId": ds.get("claudeSessionId"),
                          "label": e.get("label")})
        elif any(k in eid for k in ("tab", "zone", "terminal")):
            els.append({"id": eid, "label": (e.get("label") or "")[:100]})
    return {"activeTab": snap.get("activeTab"), "pages": ui_action(inst, "terminal-pages", "list-pages"),
            "mountedTerminals": ui_action(inst, "terminal-page", "list-terminals"),
            "zoneHeaders": zones, "focusedElements": focused, "elements": els}


def make_pages(inst: Instance, n_pages: int) -> tuple[list[str], str]:
    """Create pages through the UI the way an operator does (the + button), so
    they exist in the webview's roster. Returns (row pages, empty active page)."""
    ui_click(inst, "nav:terminal")
    time.sleep(2)
    for _ in range(n_pages):  # n_pages - 1 row pages beside `default`, plus one empty page
        ui_click(inst, "button-add-terminal-page-0")
        time.sleep(1)
    roster = ui_action(inst, "terminal-pages", "list-pages") or []
    ids = [p["pageId"] for p in roster] if isinstance(roster, list) else []
    if len(ids) < n_pages + 1:
        raise SystemExit(f"page roster has {len(ids)} pages after creating {n_pages}: {roster}")
    return ids[:n_pages], ids[-1]


def live_panes(inst: Instance) -> int:
    """Live terminals the runner reports (GET /terminals), or -1 when unreadable."""
    st, body = http("GET", inst.url + "/terminals")
    d = data_of(body) if st == 200 else None
    rows = d.get("terminals", d.get("sessions")) if isinstance(d, dict) else d
    return sum(1 for t in rows if t.get("isAlive", True)) if isinstance(rows, list) else -1


def save(inst: Instance, name: str, obj) -> None:
    (inst.out / name).write_text(json.dumps(obj, indent=2, default=str))


def create_session(inst: Instance, label: str, page: str) -> dict:
    sid = str(uuid.uuid4())
    wd = inst.work / label
    wd.mkdir(parents=True, exist_ok=True)
    status, body = http("POST", f"{inst.url}/terminals", {
        "title": f"harness {label}", "working_dir": str(wd), "page_id": page,
        "account": str(inst.account_dir), "initial_command": f"claude --resume {sid}"})
    d = (data_of(body) or {}) if status == 200 else {}
    tid = d.get("id")
    return {"label": label, "session_id": sid, "page_id": page, "working_dir": str(wd),
            "terminal_id": tid, "shell_pid": d.get("pid"), "create_status": status,
            "create_error": None if tid else body}


def row_of(inst: Instance, sid: str) -> dict | None:
    st, body = http("GET", f"{inst.url}/control/sessions/restore-health?include=all", timeout=30)
    rows = (data_of(body) or {}).get("sessions", []) if st == 200 else []
    mine = [r for r in rows if r.get("claudeSessionId") == sid]
    return mine[0] if mine else None


def close_by_shell_kill(inst: Instance, sess: dict, timeout: float = 150) -> dict:
    """SIGKILL the session's pane shell (a pid this harness's runner spawned) and
    wait for the lifecycle poll to close the row — an unexpected pane death,
    which leaves the row closed, unfinished and door-resumable. (Ending only
    the stub claude does NOT close the row: the shell keeps the pane alive.)"""
    pid = sess.get("shell_pid")
    if pid:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    t0 = time.time()
    row = None
    while time.time() - t0 < timeout:
        row = row_of(inst, sess["session_id"])
        if row and row.get("state") == "closed":
            break
        time.sleep(3)
    return {"shell_pid_killed": pid, "row": row, "secs": round(time.time() - t0, 1)}


def ledger_zone(inst: Instance, sid: str) -> dict | None:
    st, body = http("GET", f"{inst.url}/control/sessions/ledger")
    cur = ((data_of(body) or {}).get("current") or {}).get("sessions", []) if st == 200 else []
    e = next((x for x in cur if x.get("claudeSessionId") == sid), None)
    return {k: e.get(k) for k in ("terminalId", "pageId", "zoneIndex")} if e else None


def placement_probe(inst: Instance, active_page: str, other_page: str) -> dict:
    """D5's risk, retired first: where does a door-resumed pane land, and does
    focus move? Two closed rows: one on the page on screen (mounted, so its
    zones are readable) behind a keeper pane, one on a page not on screen."""
    keeper = create_session(inst, "probe-keeper", active_page)
    on_screen = create_session(inst, "probe-on-screen", active_page)
    off_screen = create_session(inst, "probe-off-screen", other_page)
    trio = [keeper, on_screen, off_screen]
    wait_rows(inst, {x["session_id"] for x in trio if x["terminal_id"]}, 90)
    time.sleep(5)
    res: dict = {"sessions": trio, "view_seeded": ui_view(inst),
                 "recorded_before_close": {x["label"]: ledger_zone(inst, x["session_id"]) for x in trio}}
    res["close"] = {x["label"]: close_by_shell_kill(inst, x) for x in (on_screen, off_screen)}
    time.sleep(3)
    res["view_before"] = ui_view(inst)
    res["recorded_before_resume"] = {x["label"]: ledger_zone(inst, x["session_id"]) for x in trio}
    st, body = http("POST", f"{inst.url}/control/sessions/resume",
                    {"ids": [on_screen["session_id"], off_screen["session_id"]]}, timeout=120)
    res["resume_status"], res["resume"] = st, body
    time.sleep(8)
    res["view_after"] = ui_view(inst)
    res["recorded_after_resume"] = {x["label"]: ledger_zone(inst, x["session_id"]) for x in trio}
    st, terms = http("GET", inst.url + "/terminals")
    res["terminals_after"] = [{k: t.get(k) for k in ("id", "pageId", "title", "isAlive")}
                              for t in ((data_of(terms) or {}).get("terminals") or [])]
    return res


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--runner-bin", required=True)
    ap.add_argument("--port", type=int, default=9891)
    ap.add_argument("--instance-name", default="restore-harness")
    ap.add_argument("--n", type=int, default=12)
    ap.add_argument("--pages", type=int, default=3)
    ap.add_argument("--pty-holder", choices=("on", "off"), default="off")
    ap.add_argument("--kill-holders", action="store_true",
                    help="also SIGKILL this instance's pty holders (a control-group stop)")
    ap.add_argument("--restore-window", type=int, default=120)
    ap.add_argument("--settle-minutes", type=float, default=0,
                    help="read the at-rest census again this long after the second boot")
    ap.add_argument("--no-placement-probe", action="store_true")
    ap.add_argument("--boot-timeout", type=int, default=300)
    ap.add_argument("--display", default=os.environ.get("DISPLAY", ":99"))
    ap.add_argument("--xauthority", default=os.environ.get("XAUTHORITY"))
    ap.add_argument("--out", required=True)
    ap.add_argument("--base", help="temp root (default: a new dir under $XDG_RUNTIME_DIR, else $TMPDIR)")
    ap.add_argument("--keep", action="store_true", help="leave the instance running and the root")
    a = ap.parse_args()
    if a.port == 9876:
        raise SystemExit("refusing :9876 — that is the primary runner")

    # A holder pane's socket lives at <root>/home/runner/pty-panes/<terminal id>.sock,
    # and a unix socket path is capped at 107 bytes: a long --base makes every
    # holder spawn fail and fall back, visibly, to an in-process PTY.
    short_tmp = os.environ.get("XDG_RUNTIME_DIR") or None
    base = Path(a.base) if a.base else Path(tempfile.mkdtemp(prefix="rrh-", dir=short_tmp))
    sock_len = len(str(base / "root/home/runner/pty-panes")) + len("/" + "0" * 36 + ".sock")
    if a.pty_holder == "on" and sock_len > 107:
        raise SystemExit(f"--base too long for holder sockets ({sock_len} > 107 bytes): {base}")
    inst = Instance(a, base)
    inst.prepare()
    summary: dict = {"args": vars(a), "base": str(base), "started_at": time.time(), "census": []}
    try:
        summary["boot1"] = inst.boot("boot1")
        time.sleep(10)
        summary["census"].append(thread_census(inst, "boot1_0_panes", 0))
        row_pages, empty_page = make_pages(inst, a.pages)
        summary["pages"] = {"row_pages": row_pages, "empty_active_page": empty_page}
        seeded = []
        for i in range(a.n):
            seeded.append(create_session(inst, f"sess-{i:02d}", row_pages[i % len(row_pages)]))
            if i + 1 == a.n // 2:
                wait_rows(inst, {s["session_id"] for s in seeded if s["terminal_id"]}, 60)
                summary["census"].append(thread_census(inst, "boot1_half", len(seeded), time.time() + 1))
        save(inst, "seeded.json", seeded)
        ids = {s["session_id"] for s in seeded if s["terminal_id"]}
        rows = wait_rows(inst, ids, timeout=90)
        time.sleep(5)
        summary["census"].append(thread_census(inst, "boot1_n", len(seeded), time.time() + 1))
        summary["pre_kill"] = {
            "seeded": len(seeded), "created": len(ids),
            "stub_launches": len(stub_records(inst)),
            "rows_confirmed": sum(1 for r in rows if r.get("confirmed")),
            "view": ui_view(inst)}
        for path, name in (("/control/sessions/restore-health", "pre-restore-health.json"),
                           ("/control/sessions/ledger", "pre-ledger.json"),
                           ("/terminals", "pre-terminals.json")):
            save(inst, name, http("GET", inst.url + path)[1])
        launches_before = len(stub_records(inst))
        # The lifecycle poll persists the ledger on change; give it a few ticks.
        time.sleep(15)
        summary["kill"] = inst.sigkill(a.kill_holders)
        log(f"killed: {summary['kill']}")

        summary["boot2"] = inst.boot("boot2")
        boot2_at = time.time()
        time.sleep(10)
        summary["census"].append(thread_census(inst, "boot2_at_rest_boot", 0))
        log(f"restore window {a.restore_window}s, touching no page")
        time.sleep(a.restore_window)
        reads = {}
        for path, name in (("/control/sessions/ledger", "post-ledger.json"),
                           ("/control/sessions/restore-health", "post-restore-health.json"),
                           ("/control/sessions/restore-census", "post-restore-census.json"),
                           ("/terminals", "post-terminals.json")):
            st, body = http("GET", inst.url + path)
            reads[path] = st
            save(inst, name, body)
        summary["post_view"] = ui_view(inst)
        recs = stub_records(inst)
        summary["post"] = {"read_status": reads,
                           "stub_launches_after_kill_before_resume": len(recs) - launches_before}
        # By id first: `all: true` selects CLOSED rows only, so it never names
        # the open, dead-bound rows at all; naming them shows the refusal.
        st, by_id = http("POST", f"{inst.url}/control/sessions/resume",
                         {"ids": sorted(ids)}, timeout=300)
        reads["POST /control/sessions/resume ids"] = st
        save(inst, "post-resume-ids.json", by_id)
        st, resume = http("POST", f"{inst.url}/control/sessions/resume", {"all": True}, timeout=300)
        reads["POST /control/sessions/resume all"] = st
        save(inst, "post-resume-all.json", resume)
        time.sleep(10)
        recs = stub_records(inst)
        summary["post"]["stub_launches_after_resume_all"] = len(recs) - launches_before
        save(inst, "post-resume-all-restore-health.json", restore_rows(inst))
        summary["census"].append(thread_census(inst, "boot2_after_resume_all", live_panes(inst)))
        if not a.no_placement_probe:
            summary["placement_probe"] = placement_probe(inst, empty_page, row_pages[1 % len(row_pages)])
            save(inst, "placement-probe.json", summary["placement_probe"])
        save(inst, "stub-records.json", stub_records(inst))
        if a.settle_minutes:
            time.sleep(max(0.0, a.settle_minutes * 60 - (time.time() - boot2_at)))
            summary["census"].append(thread_census(inst, "boot2_settled", live_panes(inst)))
    finally:
        summary["finished_at"] = time.time()
        save(inst, "summary.json", summary)
        if not a.keep:
            inst.stop()
            shutil.rmtree(base, ignore_errors=True)
            log(f"stopped and removed {base}")
        else:
            log(f"--keep: instance pid {inst.proc.pid if inst.proc else None}, root {base}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
