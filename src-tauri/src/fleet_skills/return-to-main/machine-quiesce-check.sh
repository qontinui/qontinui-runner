#!/usr/bin/env bash
# machine-quiesce-check.sh - is THIS machine quiet enough for an unattended job
# to move checkouts? QUIET / BUSY / UNKNOWN, with every signal it read.
#
# Plan 2026-09-13-nightly-return-to-main-sweep, Phase 2b. The consumer is the
# `/return-to-main` skill (Phase 4): UNKNOWN or a non-overridable BUSY puts the
# night in shadow; an overridable-only BUSY is where the skill applies D3's
# per-repo override using `per_repo` and `newest_transcript_write`.
#
# WHAT IT READS (each one becomes an entry in `probes[]`):
#   census               scripts/session-census.sh --json -- agent, runner and
#                        cargo/rustc processes, self excluded by ANCESTRY
#   runner_instances     supervisor GET 127.0.0.1:9875/runners; a REFUSED
#                        connect means no supervisor on this box ->
#                        not_applicable, and 127.0.0.1:9876 alone is probed
#   restart_readiness:N  GET 127.0.0.1:N/restart-readiness per instance; only
#                        `safe_to_restart: true`, or a false whose every
#                        itemised blocking process is THIS job's own, is quiet.
#                        A false the check cannot attribute (nothing itemised
#                        is left AND nothing was discounted as self), or one
#                        whose reason begins "UNKNOWN" (the runner's own
#                        fail-closed arm, restart_readiness.rs build_verdict,
#                        which can answer false with every count at 0), is
#                        UNKNOWN -- never discounted into quiet
#   runner_coverage      census runner processes vs instances that answered
#   supervisor_builds    GET 127.0.0.1:9875/builds; refused -> not_applicable
#   cargo_guard_lock     <root>/*/.build-state/locks/cargo.lockdir mtime
#                        (fresh = touched within $CARGO_GUARD_LOCK_STALE_AFTER,
#                        default 120 s); none present -> not_applicable
#   coord_sessions       coord GET /coord/sessions/fleet?device_id=<this device>
#                        -- `active` rows other than this job's own
#   custody              <repo>/.git/qontinui-custody.d/*.json `last_seen`
#   transcripts          newest *.jsonl under every account's projects/
#
# THE CLASSES (plan D3). A signal that could not be read is UNKNOWN, and
# UNKNOWN never acts
# [policy: verification-and-evidence silent-empty-is-unknown].
#   blocking     (BUSY, never overridable) runner-hosted / AI sessions other
#                than self; running builds (cargo/rustc process, fresh
#                cargo-guard lock, supervisor slot `building`) -- EXCEPT a
#                cargo/rustc the census marks `ide_check` (rust-analyzer's own
#                flycheck: its nearest non-build ancestor is rust-analyzer).
#                That one is IGNORED and REPORTED (`ide_checks_ignored`, named
#                in the census probe detail), neither blocking nor overridable:
#                the override's idle signals are Claude Code's own and mean
#                nothing for an IDE, and the sweep runs no cargo. A person's
#                build in an IDE terminal (cargo <- bash <- code) still
#                blocks. Residual: an overrideCommand wrapping cargo in a shell
#                reads as a build (BUSY, the safe side). Plan
#                2026-09-13-heartbeat-stop-python3-stub-ide-cargo-quiesce D2;
#                `active` coord
#                rows for this device other than self; EXTERNAL agents of any
#                family other than Claude Code (codex, pi, a node-hosted
#                non-Claude agent) -- class external_agent_no_idle_signal.
#   overridable  (BUSY, overridable) external CLAUDE CODE processes other than
#                self, and nothing else. The override judges idleness by
#                custody records (written by Claude Code's Stop hook) and
#                transcripts (under Claude's projects/); no other agent family
#                writes either, so a working Codex would always read idle.
# An unreadable signal is UNKNOWN: a `node` whose command line the census could
# not read (census `unreadable_nodes`) makes the census probe unknown. The one
# exception is a node the census can PROVE is a Windows service, by an
# ALLOWLIST of ancestry shapes (session-census.sh's header has the full rule):
# in session 0, either `services.exe <- node [<- node]*` with the top node the
# process of exactly one registered service, or
# `services.exe <- W <- node [<- node]*` with W directly under services.exe,
# likewise the process of exactly one registered service (Win32_Service, read
# in the same PowerShell pass), and not a multi-purpose / remote-execution host
# (svchost, dllhost, WmiPrvSE, wsmprovhost, PSEXESVC) or a logon, task, shell
# or terminal host. Everything else -- svchost anywhere (so a scheduled task),
# WinRM, remote WMI, PsExec, a second non-node intermediate, a runner or agent
# ancestor, an unwalkable chain, a missing SessionId, a null CreationDate, a
# failed Win32_Service read -- is NOT proof and reads UNKNOWN; the probe detail
# quotes the census's refusal reason. A proven service is reported as
# `service_nodes_ignored`, named in the probe detail, and moves no verdict --
# otherwise a box running any node service as SYSTEM would be UNKNOWN every
# night. Residual, stated rather than hidden, and wider than "remote
# execution": ANY registered single-purpose service directly under services.exe
# and off the refusal list whose process IS node.exe or STARTS node.exe reads
# as a service, with every node descended from it through node-only hops --
# including agent-hosting services and remote-dev servers (e.g. code-server, a
# node app registered as a service itself, or an Agent SDK app installed with
# nssm as LocalSystem running Claude Code as `node cli.js` under its own node),
# which then read QUIET. It cannot be told apart without reading the command
# line (needs elevation), and every such setup needs an administrator to
# register the service. Deliberate trade: a node service wrapped by a `.bat` /
# `cmd` reads UNKNOWN every night.
# Verdict: any blocking -> BUSY; else any UNKNOWN probe -> UNKNOWN; else any
# overridable -> BUSY (override_eligible: true); else QUIET.
#
# NOT APPLICABLE IS NOT UNKNOWN. The supervisor and cargo-guard are dev-box
# tooling; a user's box has neither. A refused connect on :9875 and an absent
# lockdir are `not_applicable`. Only a TIMEOUT, a non-2xx or an unparseable
# body is UNKNOWN -- a blackholed port is a timeout, never a refusal.
#
# COORD CREDENTIAL (never on argv, never printed). Cascade:
#   1. the session's coord-mcp proxy (<root>/.mcp.json mcpServers.coord-mcp
#      url + headers). Measured 2026-09-13 on nomad: it answers
#      `404 No route for GET /coord-mcp/coord/sessions/fleet` -- it is a
#      JSON-RPC door and forwards no REST path -- so this rung falls through
#      today and is kept for the build that forwards it.
#   2. coord's anonymous bootstrap `POST /agents/credential {"device_id"}` ->
#      a device-subject agent JWT (measured 200 on nomad 2026-09-13).
# Headers are staged in a mode-600 file inside a mode-700 temp dir and passed
# as `curl -H @file`; the dir is removed on exit.
#
# Usage: machine-quiesce-check.sh [--root DIR] [--json] [--self-session-id ID]
#                                 [--readiness-timeout S] [--http-timeout S]
#   --root DIR          workspace root (default: resolved as the sweep does;
#                       $QONTINUI_ROOT wins)
#   --json              one JSON object on stdout:
#                       {verdict, override_eligible, blocking[], overridable[],
#                        ide_checks_ignored[],
#                        per_repo{<repo>:{last_custody_seen, last_custody_age_s,
#                        custody_status, newest_transcript_write,
#                        newest_transcript_age_s}}, newest_transcript_write,
#                        self{pids, agent_pids, resolved, session_id},
#                        probes[{name, status, detail}], as_of, root}
#   --self-session-id   this job's Claude session id (default
#                       $CLAUDE_CODE_SESSION_ID): discounts its coord row, its
#                       custody records and its own transcript
#
# Fixture inputs (tests; no live system needed):
#   QMQC_FIXTURE_DIR    canned HTTP: $DIR/http/<name>.status holds `200` (or any
#                       code), `refused` or `timeout`; <name>.body the body.
#                       Names: sup_runners, sup_builds, readiness_<port>,
#                       coord_proxy_fleet_<page>, coord_credential,
#                       coord_fleet_<page>
#   QMQC_CENSUS_BIN     the census to run (default: beside this script)
#   QMQC_LIB_DIR        where hook-json.sh is (default: lib/ beside this script)
#   QMQC_NOW            epoch seconds used as "now"
#   QMQC_COORD_URL      coord base (default $COORD_HTTP_URL, then
#                       https://coord.qontinui.io)
#   SESSION_CENSUS_*    passed through to the census (see its header)
#
# Exit: 0 QUIET, 1 BUSY, 3 UNKNOWN, 4 usage.
set -u

ROOT_ARG=""; JSON=0
SELF_SID="${CLAUDE_CODE_SESSION_ID:-}"
READINESS_TIMEOUT=60; HTTP_TIMEOUT=20
usage() { sed -n '2,/^set -u$/{/^set -u$/d;p;}' "$0"; }
while [ $# -gt 0 ]; do
  case "$1" in
    --root) shift; [ $# -gt 0 ] || { echo "machine-quiesce-check: --root needs a directory" >&2; exit 4; }; ROOT_ARG="$1" ;;
    --json) JSON=1 ;;
    --self-session-id) shift; [ $# -gt 0 ] || { echo "machine-quiesce-check: --self-session-id needs a value" >&2; exit 4; }; SELF_SID="$1" ;;
    --readiness-timeout) shift; case "${1:-}" in ''|*[!0-9]*) echo "machine-quiesce-check: --readiness-timeout needs seconds" >&2; exit 4 ;; esac; READINESS_TIMEOUT="$1" ;;
    --http-timeout) shift; case "${1:-}" in ''|*[!0-9]*) echo "machine-quiesce-check: --http-timeout needs seconds" >&2; exit 4 ;; esac; HTTP_TIMEOUT="$1" ;;
    -h|--help) usage; exit 0 ;;
    *) echo "machine-quiesce-check: unknown argument: $1" >&2; exit 4 ;;
  esac
  shift
done
case "$SELF_SID" in
  *[!0-9A-Za-z-]*) echo "machine-quiesce-check: --self-session-id must be a session UUID" >&2; exit 4 ;;
esac

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CENSUS="${QMQC_CENSUS_BIN:-$SCRIPT_DIR/session-census.sh}"
COORD="${QMQC_COORD_URL:-${COORD_HTTP_URL:-https://coord.qontinui.io}}"; COORD="${COORD%/}"

LIB_DIR="${QMQC_LIB_DIR:-$SCRIPT_DIR/lib}"

# Sets PY (an argv prefix: the interpreter, plus `-3` for the py launcher)
# through the shared shim, lib/hook-json.sh. The shim never executes a
# WindowsApps App Execution Alias except under `timeout`; the loop this replaced
# ran `python3 -c` unbounded, and the Python Install Manager's alias HANGS, so
# this nightly gate hung before reading a single signal (plan
# 2026-09-13-heartbeat-stop-python3-stub-ide-cargo-quiesce, D1 re-vet).
# $PYTHON, when set, is the ONLY candidate.
resolve_python() {
  { [ -r "$LIB_DIR/hook-json.sh" ] && . "$LIB_DIR/hook-json.sh" && hook_shim_python3 --require 'assert sys.version_info >= (3, 6); import json'; } || return 1
  PY=("$HOOK_PY3_EXE"); [ -n "${HOOK_PY3_ARG:-}" ] && PY+=("$HOOK_PY3_ARG")
  return 0
}
native() {
  if command -v cygpath >/dev/null 2>&1; then cygpath -m "$1"; else printf '%s' "$1"; fi
}
# curl.exe opens the -o target and the -H @file header path itself, so both must
# be native-spelled: under an inherited MSYS_NO_PATHCONV=1 a POSIX path reaches
# curl.exe unconverted and the open fails on a file that exists (lint check #9).
curl_path() {
  if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf '%s' "$1"; fi
}

resolve_python || {
  # Nothing below can be read without an interpreter; say so in the contract's
  # own shape rather than as prose a consumer would have to parse.
  printf '{"verdict":"UNKNOWN","override_eligible":false,"blocking":[],"overridable":[],"per_repo":{},"self":{"pids":[]},"probes":[{"name":"python","status":"unknown","detail":"no working python3/python interpreter"}]}\n'
  exit 3
}

# ---- workspace root: the same marker probe as return-to-main-sweep.sh ------
_q_is_root() {
  case "$1" in ""|/|//) return 1 ;; esac
  [ -e "$1/qontinui-claude-config/.git" ] && [ -f "$1/.claude/settings.json" ]
}
_q_resolve_root() {
  local d
  if [ -n "$ROOT_ARG" ]; then [ -d "$ROOT_ARG" ] || return 1; (cd "$ROOT_ARG" && pwd); return 0; fi
  if [ -n "${QONTINUI_ROOT:-}" ] && _q_is_root "$QONTINUI_ROOT"; then (cd "$QONTINUI_ROOT" && pwd); return 0; fi
  for d in "$PWD" "${CLAUDE_PROJECT_DIR:-}" "$SCRIPT_DIR"; do
    while [ -n "$d" ]; do
      _q_is_root "$d" && { (cd "$d" && pwd); return 0; }
      case "$d" in ""|/|//) break ;; esac
      d="${d%/*}"; [ -z "$d" ] && d=/
    done
  done
  return 1
}
ROOT="$(_q_resolve_root)" || { echo "machine-quiesce-check: cannot resolve the workspace root (pass --root, or set \$QONTINUI_ROOT)" >&2; exit 4; }

WORK="$(mktemp -d 2>/dev/null)" || { echo "machine-quiesce-check: mktemp failed" >&2; exit 3; }
chmod 700 "$WORK" 2>/dev/null
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/http"
umask 077

# http_fetch NAME METHOD URL [HEADER_FILE] [JSON_BODY] [TIMEOUT]
# Leaves $WORK/http/NAME.body and NAME.status = `http <code>` | `refused` |
# `timeout` | `error <why>`. Nothing about the request reaches argv except the
# URL and a non-secret body.
http_fetch() {
  local name="$1" method="$2" url="$3" hdr="${4:-}" data="${5:-}" tmo="${6:-$HTTP_TIMEOUT}"
  local out="$WORK/http/$name.body" st="$WORK/http/$name.status" code rc
  : >"$out"
  if [ -n "${QMQC_FIXTURE_DIR:-}" ]; then
    local f="$QMQC_FIXTURE_DIR/http/$name"
    printf '%s %s\n' "$method" "$url" >>"$QMQC_FIXTURE_DIR/requests.log"
    if [ ! -f "$f.status" ]; then echo "error fixture-missing:$name" >"$st"; return 0; fi
    code="$(tr -d ' \r\n' <"$f.status")"
    case "$code" in
      refused|timeout) echo "$code" >"$st" ;;
      [0-9][0-9][0-9]) [ -f "$f.body" ] && cp "$f.body" "$out"; echo "http $code" >"$st" ;;
      *) echo "error bad-fixture:$code" >"$st" ;;
    esac
    return 0
  fi
  local out_p hdr_p
  out_p="$(curl_path "$out")"
  local args=(-s -S -o "$out_p" -w '%{http_code}' --connect-timeout 5 -m "$tmo" -X "$method")
  if [ -n "$hdr" ]; then hdr_p="$(curl_path "$hdr")"; args+=(-H "@$hdr_p"); fi
  [ -n "$data" ] && args+=(-H 'Content-Type: application/json' --data "$data")
  code="$(curl "${args[@]}" "$url" 2>"$WORK/http/$name.err")"; rc=$?
  case $rc in
    0) echo "http $code" >"$st" ;;
    7) echo "refused" >"$st" ;;
    28) echo "timeout" >"$st" ;;
    *) echo "error curl_rc=$rc $(head -c 160 "$WORK/http/$name.err" | tr '\n' ' ')" >"$st" ;;
  esac
}
http_code() { sed -n 's/^http \([0-9]*\)$/\1/p' "$WORK/http/$1.status" 2>/dev/null; }

cat >"$WORK/qmqc.py" <<'PYEOF'
import glob, json, os, re, sys, time
from datetime import datetime, timezone
from urllib.parse import quote

NOW = float(os.environ.get("QMQC_NOW") or time.time())


def load(path):
    try:
        with open(path, "rb") as fh:
            return json.loads(fh.read().decode("utf-8", "replace").lstrip("\ufeff"))
    except (OSError, ValueError):
        return None


def status(work, name):
    try:
        with open(os.path.join(work, "http", name + ".status")) as fh:
            return fh.read().strip()
    except OSError:
        return "error not-fetched"


def iso(ts):
    return datetime.fromtimestamp(ts, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ") if ts is not None else None


def parse_iso(s):
    if not isinstance(s, str):
        return None
    m = re.match(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(?:\.\d+)?(Z|[+-]\d\d:?\d\d)?$", s.strip())
    if not m:
        return None
    base = datetime.strptime(m.group(1), "%Y-%m-%dT%H:%M:%S").replace(tzinfo=timezone.utc).timestamp()
    tz = m.group(2)
    if tz and tz != "Z":
        sign = 1 if tz[0] == "+" else -1
        t = tz[1:].replace(":", "")
        base -= sign * (int(t[:2]) * 3600 + int(t[2:]) * 60)
    return base


def cmd_ports(work):
    """Print the runner ports to probe (one per line) and the supervisor state."""
    st = status(work, "sup_runners")
    ports, listed = [], []
    if st.startswith("http 2"):
        doc = load(os.path.join(work, "http", "sup_runners.body"))
        if isinstance(doc, list):
            for r in doc:
                if isinstance(r, dict) and r.get("running") and isinstance(r.get("port"), int):
                    listed.append(r["port"])
    ports = sorted(set(listed) | {9876})
    print(" ".join(str(p) for p in ports))


def cmd_device_id(home):
    v = (os.environ.get("QONTINUI_MACHINE_ID") or "").strip()
    if not v:
        doc = load(os.path.join(home, ".qontinui", "machine.json"))
        if isinstance(doc, dict):
            v = str(doc.get("device_id") or doc.get("machine_id") or "").strip()
    if re.match(r"^[0-9a-fA-F-]{36}$", v or ""):
        print(v)


def cmd_proxy(mcp_json, hdr_out):
    """Write the coord-mcp proxy's headers to hdr_out; print its URL base."""
    doc = load(mcp_json)
    srv = ((doc or {}).get("mcpServers") or {}).get("coord-mcp") if isinstance(doc, dict) else None
    if not isinstance(srv, dict) or not srv.get("url"):
        return
    hdrs = srv.get("headers") or {}
    with open(hdr_out, "w") as fh:
        for k, v in hdrs.items():
            if k in ("Authorization", "X-Coord-Mcp-Proxy-Key") and isinstance(v, str) and "\n" not in v:
                fh.write("%s: %s\n" % (k, v))
    print(str(srv["url"]).rstrip("/"))


def cmd_cred(body, hdr_out):
    doc = load(body)
    tok = (doc or {}).get("token") if isinstance(doc, dict) else None
    if isinstance(tok, str) and tok and "\n" not in tok:
        with open(hdr_out, "w") as fh:
            fh.write("Authorization: Bearer %s\n" % tok)
        print("ok")


def cmd_next_cursor(body):
    doc = load(body)
    cur = doc.get("nextCursor") if isinstance(doc, dict) else None
    if isinstance(cur, str) and cur:
        print(quote(cur, safe=""))


# --------------------------------------------------------------------------
def cmd_final(work, census_path, census_rc, root, root_native, self_sid, device_id, script_dir, mode):
    probes, blocking, overridable, ide_ignored = [], [], [], []

    def probe(name, st, detail):
        probes.append({"name": name, "status": st, "detail": detail})

    # ---- census -----------------------------------------------------------
    census = load(census_path)
    self_pids, self_agents, self_resolved = set(), set(), False
    census_runners = 0
    unreadable = census.get("unreadable_nodes") if isinstance(census, dict) else None
    if census_rc not in ("0", "1", "3") or not isinstance(census, dict) or not isinstance(census.get("processes"), list):
        probe("census", "unknown", "session-census.sh exit %s, output %s" % (
            census_rc, "unparseable" if census is None else "missing processes[]"))
    elif not isinstance(unreadable, dict) or not isinstance(unreadable.get("count"), int):
        probe("census", "unknown", "session-census.sh output has no unreadable_nodes count -- an absent report is not zero")
    else:
        s = census.get("self") or {}
        self_pids = set(s.get("pids") or [])
        self_agents = set(s.get("agent_pids") or [])
        self_resolved = bool(s.get("resolved"))
        n_ext = n_run = n_build = n_noidle = 0
        for p in census["processes"]:
            kind, origin = p.get("kind"), p.get("origin")
            if origin == "self":
                continue
            ent = {"pid": p.get("pid"), "kind": kind, "name": p.get("name"), "age_s": p.get("age_s"),
                   "ancestry": (p.get("ancestry") or [])[:4], "source": "census"}
            if kind in ("claude", "codex", "pi"):
                if origin == "runner":
                    ent["class"] = "runner_session"; blocking.append(ent); n_run += 1
                elif kind == "claude":
                    ent["class"] = "external_session"; overridable.append(ent); n_ext += 1
                else:
                    # Both idle signals the override reads are Claude Code's
                    # own, so this family would read idle while working.
                    ent["class"] = "external_agent_no_idle_signal"
                    ent["reason"] = "no idle signal exists for this agent family"
                    blocking.append(ent); n_noidle += 1
            elif kind == "build" and p.get("ide_check") is True:
                # rust-analyzer's own flycheck (plan D2): the sweep runs no
                # cargo, and a real build still blocks through its own row, the
                # cargo-guard lock and the supervisor slot. Reported, not dropped.
                ide_ignored.append(ent)
            elif kind == "build":
                ent["class"] = "build_process"; ent["launched_by"] = p.get("launched_by")
                blocking.append(ent); n_build += 1
            elif kind == "runner" and not any(a.startswith("qontinui-runner") for a in (p.get("ancestry") or [])):
                census_runners += 1
        detail = "%s: %d runner-hosted, %d external Claude Code, %d external other-family, %d build process(es), %d runner instance(s); self %s" % (
            census.get("source"), n_run, n_ext, n_noidle, n_build, census_runners,
            ("agent pid(s) " + ",".join(str(x) for x in sorted(self_agents))) if self_agents else
            ("resolved, no agent on the chain" if self_resolved else "UNRESOLVED"))
        if ide_ignored:
            detail += "; %d rust-analyzer check process(es) ignored (pids %s)" % (
                len(ide_ignored), ",".join(str(e["pid"]) for e in ide_ignored[:12]))
        # Transparency only: a census predating the field reports every
        # unreadable node in unreadable_nodes, so its absence hides nothing.
        svc = census.get("service_nodes_ignored")
        if isinstance(svc, dict) and isinstance(svc.get("count"), int) and svc["count"] > 0:
            detail += "; %d unreadable node(s) in Windows session 0 (services) ignored (pids %s)" % (
                svc["count"], ",".join(str(x) for x in (svc.get("pids") or [])[:12]))
        if unreadable["count"] > 0:
            # Say WHY a session-0 node was not excused, so a nightly UNKNOWN on
            # a box running a node service names what to change.
            refused = [u for u in (unreadable.get("processes") or []) if isinstance(u, dict) and u.get("not_a_service")]
            why = ("; session-0 node(s) not proven Windows services: %s" % "; ".join(
                "%s: %s" % (u.get("pid"), u["not_a_service"]) for u in refused[:3])) if refused else ""
            probe("census", "unknown", "%d node process(es) with an unreadable command line (pids %s) -- cannot rule out an agent; %s%s" % (
                unreadable["count"], ",".join(str(x) for x in (unreadable.get("pids") or [])[:12]), detail, why))
        else:
            probe("census", "ok", detail)

    # ---- runner instances ---------------------------------------------------
    st = status(work, "sup_runners")
    sup_ports = set()
    if st == "refused":
        probe("runner_instances", "not_applicable", "no supervisor on 127.0.0.1:9875 (connection refused); probing 127.0.0.1:9876 only")
    elif st.startswith("http 2"):
        doc = load(os.path.join(work, "http", "sup_runners.body"))
        if isinstance(doc, list):
            sup_ports = {r["port"] for r in doc if isinstance(r, dict) and r.get("running") and isinstance(r.get("port"), int)}
            probe("runner_instances", "ok", "supervisor lists %d running runner(s): %s" % (
                len(sup_ports), ",".join(str(x) for x in sorted(sup_ports)) or "none"))
        else:
            probe("runner_instances", "unknown", "supervisor /runners body unparseable")
    else:
        probe("runner_instances", "unknown", "supervisor /runners: %s" % st)

    # ---- restart-readiness per instance ----------------------------------
    answered = 0
    for f in sorted(glob.glob(os.path.join(work, "http", "readiness_*.status"))):
        port = int(re.search(r"readiness_(\d+)\.status$", f.replace("\\", "/")).group(1))
        name = "restart_readiness:%d" % port
        st = status(work, "readiness_%d" % port)
        if st == "refused":
            if port in sup_ports:
                probe(name, "unknown", "the supervisor lists this runner as running but :%d refused" % port)
            else:
                probe(name, "not_applicable", "nothing listening on 127.0.0.1:%d" % port)
            continue
        if not st.startswith("http 2"):
            probe(name, "unknown", "/restart-readiness: %s" % st)
            continue
        doc = load(os.path.join(work, "http", "readiness_%d.body" % port))
        if not isinstance(doc, dict) or not isinstance(doc.get("safe_to_restart"), bool):
            probe(name, "unknown", "/restart-readiness body unparseable or has no boolean safe_to_restart")
            continue
        answered += 1
        if doc["safe_to_restart"] is True:
            probe(name, "ok", "safe_to_restart: true")
            continue
        reason = str(doc.get("reason") or "")
        # The runner's fail-closed arm answers false with "UNKNOWN, so treated
        # as unsafe: ..." while every count can read 0 -- not attributable.
        fail_closed = reason.lstrip().upper().startswith("UNKNOWN")
        ts, ai = doc.get("terminal_sessions"), doc.get("ai_sessions")
        if not isinstance(ts, dict) or not isinstance(ai, dict):
            probe(name, "unknown", "safe_to_restart false with a null plane (fail-closed): %s" % reason[:200])
            continue
        lc = doc.get("live_claude")
        if isinstance(lc, dict) and isinstance(lc.get("blocking"), int):
            listed = {}
            hl = doc.get("headless_sessions") or {}
            for plane, arr in (("terminal", ts.get("processes")), ("unclassified", ts.get("unclassified_processes")),
                               ("headless", hl.get("processes") if isinstance(hl, dict) else None)):
                for p in arr or []:
                    if isinstance(p, dict) and isinstance(p.get("pid"), int) and p.get("blocks_restart", True) is not False:
                        listed.setdefault(p["pid"], (plane, p))
            residual = max(0, lc["blocking"] - len(listed))
            mine = sorted(pid for pid in listed if pid in self_pids)
            ai_procs = [p for p in (ai.get("processes") or []) if isinstance(p, dict)]
            ai_self = [p for p in ai_procs if p.get("pid") in self_pids]
            ai_left = max(0, int(ai.get("count") or 0) - len(ai_self))
            others = [(pid, v) for pid, v in sorted(listed.items()) if pid not in self_pids]
            for pid, (plane, p) in others:
                blocking.append({"class": "runner_session", "source": "restart-readiness", "port": port, "pid": pid,
                                 "plane": plane, "session_id": p.get("session_id"), "age_s": p.get("age_s")})
            if residual:
                blocking.append({"class": "runner_session", "source": "restart-readiness", "port": port,
                                 "count": residual, "detail": "blocking processes the endpoint did not itemise"})
            if ai_left:
                blocking.append({"class": "ai_session", "source": "restart-readiness", "port": port, "count": ai_left})
            left = len(others) + residual + ai_left
            discounted = sorted(set(mine) | {p.get("pid") for p in ai_self})
            if fail_closed:
                probe(name, "unknown", "safe_to_restart false and the runner could not decide (fail-closed): %s" % reason[:200])
            elif left == 0 and not discounted:
                probe(name, "unknown", "safe_to_restart false with nothing to attribute it to (none itemised, none discounted as self): %s" % reason[:200])
            elif left == 0:
                probe(name, "ok", "safe_to_restart false only because of this job's own process(es) %s -- discounted" % discounted)
            else:
                probe(name, "ok", "%d blocking after discounting self %s" % (left, discounted or "[]"))
        else:
            tc, ac = int(ts.get("count") or 0), int(ai.get("count") or 0)
            if tc + ac == 0 or fail_closed:
                probe(name, "unknown", "safe_to_restart false with both planes empty or fail-closed: %s" % reason[:200])
            elif tc + ac <= len(self_agents):
                probe(name, "unknown", "this runner build predates per-process detail, and its %d counted session(s) could be this job's own %d" % (tc + ac, len(self_agents)))
            else:
                if tc:
                    blocking.append({"class": "runner_session", "source": "restart-readiness", "port": port, "count": tc,
                                     "detail": "runner build predates per-process detail; self cannot be discounted"})
                if ac:
                    blocking.append({"class": "ai_session", "source": "restart-readiness", "port": port, "count": ac})
                probe(name, "ok", "%d terminal + %d AI session(s) counted (no per-process detail on this build)" % (tc, ac))
    if census_runners > answered:
        probe("runner_coverage", "unknown", "%d qontinui-runner process(es) in the census, %d instance(s) answered /restart-readiness" % (census_runners, answered))
    else:
        probe("runner_coverage", "ok", "%d runner process(es), %d answered" % (census_runners, answered))

    # ---- supervisor builds --------------------------------------------------
    st = status(work, "sup_builds")
    if st == "refused":
        probe("supervisor_builds", "not_applicable", "no supervisor on 127.0.0.1:9875 (connection refused)")
    elif st.startswith("http 2"):
        doc = load(os.path.join(work, "http", "sup_builds.body"))
        if isinstance(doc, dict) and isinstance(doc.get("slots"), list):
            b = [s for s in doc["slots"] if isinstance(s, dict) and s.get("state") == "building"]
            for s in b:
                blocking.append({"class": "build_slot", "source": "supervisor", "slot": s.get("id"),
                                 "build_source": s.get("source"), "elapsed_secs": s.get("elapsed_secs"),
                                 "requester_id": s.get("requester_id")})
            probe("supervisor_builds", "ok", "%d slot(s), %d building" % (len(doc["slots"]), len(b)))
        else:
            probe("supervisor_builds", "unknown", "supervisor /builds body unparseable or has no slots[]")
    else:
        probe("supervisor_builds", "unknown", "supervisor /builds: %s" % st)

    # ---- cargo-guard lock ---------------------------------------------------
    stale_after = int(os.environ.get("CARGO_GUARD_LOCK_STALE_AFTER") or 120)
    locks = sorted(set(glob.glob(os.path.join(root_native, "*", ".build-state", "locks", "cargo.lockdir")) +
                       glob.glob(os.path.join(root_native, "agent-worktrees", "*", "*", ".build-state", "locks", "cargo.lockdir"))))
    if not locks:
        probe("cargo_guard_lock", "not_applicable", "no cargo.lockdir under any checkout")
    else:
        fresh = 0
        for lk in locks:
            try:
                age = int(NOW - os.stat(lk).st_mtime)
            except OSError:
                continue
            if age <= stale_after:
                fresh += 1
                try:
                    with open(os.path.join(lk, "pid")) as fh:
                        holder = fh.read().strip()
                except OSError:
                    holder = None
                blocking.append({"class": "build_lock", "source": "cargo-guard", "lockdir": lk.replace("\\", "/"),
                                 "age_s": age, "holder_pid": holder})
        probe("cargo_guard_lock", "ok", "%d lockdir(s), %d fresh (heartbeat within %ds)" % (len(locks), fresh, stale_after))

    # ---- coord session rows ---------------------------------------------------
    if not device_id:
        probe("coord_sessions", "unknown", "no device_id ($QONTINUI_MACHINE_ID or ~/.qontinui/machine.json)")
    else:
        rungs, pages, door = [], None, None
        for rung, prefix in (("proxy", "coord_proxy_fleet_"), ("bootstrap", "coord_fleet_")):
            files = sorted(glob.glob(os.path.join(work, "http", prefix + "*.status")),
                           key=lambda f: int(re.search(r"_(\d+)\.status$", f).group(1)))
            if not files:
                continue
            docs, bad = [], None
            for f in files:
                nm = os.path.basename(f)[:-len(".status")]
                st = status(work, nm)
                if not st.startswith("http 2"):
                    bad = st; break
                d = load(os.path.join(work, "http", nm + ".body"))
                if not isinstance(d, dict) or not isinstance(d.get("sessions"), list):
                    bad = "unparseable body"; break
                docs.append(d)
            if bad is None and docs:
                if docs[-1].get("nextCursor"):
                    bad = "more pages than the page cap"
                else:
                    pages, door = docs, rung
                    break
            rungs.append("%s: %s" % (rung, bad))
        cred = status(work, "coord_credential")
        if cred != "error not-fetched" and not cred.startswith("http 2"):
            rungs.append("bootstrap credential: %s" % cred)
        if pages is None:
            probe("coord_sessions", "unknown", "no coord door answered (%s)" % ("; ".join(rungs) or "none attempted"))  # unstamped-floor-ok: a per-run probe detail -- THIS run's own probe is the measurement, it only makes this run's verdict UNKNOWN, and the report carries its own per-run as_of; decided 2026-09-13
        else:
            rows = [r for d in pages for r in d["sessions"] if isinstance(r, dict)]
            bridge = all(d.get("sessionBridgeColumnPresent", True) for d in pages)
            active = mine = finished = 0
            for r in rows:
                if r.get("deviceId") and r.get("deviceId") != device_id:
                    continue
                if r.get("state") != "active":
                    continue
                active += 1
                if r.get("sessionStatus") == "finished":
                    finished += 1
                    continue
                if self_sid and r.get("claudeCodeSessionId") == self_sid:
                    mine += 1
                    continue
                blocking.append({"class": "coord_session", "source": "coord", "claude_code_session_id": r.get("claudeCodeSessionId"),
                                 "session_kind": r.get("sessionKind"), "repo": r.get("repo"), "branch": r.get("branch"),
                                 "last_heartbeat_at": r.get("lastHeartbeatAt")})
            probe("coord_sessions", "ok", "via %s: %d row(s), %d active (%d this job's own, %d finished-discounted)%s%s" % (
                door, len(rows), active, mine, finished,
                "" if bridge else "; session-id column absent, so self cannot be matched",
                ("; fell through " + "; ".join(rungs)) if rungs else ""))

    # ---- primaries: custody -------------------------------------------------
    per_repo, custody_bad = {}, []
    for d in sorted(os.listdir(root_native)):
        gd = os.path.join(root_native, d, ".git")
        if not os.path.isdir(gd):
            continue
        files = glob.glob(os.path.join(gd, "qontinui-custody.d", "*.json"))
        legacy = os.path.join(gd, "qontinui-custody.json")
        if os.path.isfile(legacy):
            files.append(legacy)
        newest, cstat = None, "none"
        for f in files:
            rec = load(f)
            if not isinstance(rec, dict):
                cstat = "unknown"; custody_bad.append("%s/%s" % (d, os.path.basename(f))); continue
            if self_sid and rec.get("session_id") == self_sid:
                continue
            ts = rec.get("last_seen_epoch")
            ts = float(ts) if isinstance(ts, (int, float)) else parse_iso(rec.get("last_seen"))
            if ts is None:
                cstat = "unknown"; custody_bad.append("%s/%s (no last_seen)" % (d, os.path.basename(f))); continue
            if newest is None or ts > newest:
                newest = ts
            if cstat == "none":
                cstat = "ok"
        per_repo[d] = {"last_custody_seen": iso(newest), "last_custody_age_s": int(NOW - newest) if newest is not None else None,
                       "custody_status": cstat, "newest_transcript_write": None, "newest_transcript_age_s": None}
    probe("custody", "unknown" if custody_bad else "ok",
          ("unreadable record(s): " + ", ".join(custody_bad[:6])) if custody_bad else
          "%d primar%s, %d with a custody record" % (len(per_repo), "y" if len(per_repo) == 1 else "ies",
                                                       sum(1 for v in per_repo.values() if v["custody_status"] == "ok")))

    # ---- transcripts ----------------------------------------------------------
    # QMQC_HOME: a native Windows python ignores $HOME, so a test needs its own
    # handle on "~" or it would read the operator's real transcripts.
    home = os.environ.get("QMQC_HOME") or os.path.expanduser("~")
    acct_root = os.environ.get("QONTINUI_CLAUDE_ACCOUNTS_ROOT") or ("C:/claude" if os.name == "nt" else home)
    cands = set(glob.glob(os.path.join(acct_root, ".claude-*")))
    # script_dir is this skill directory (<config-repo>/.claude/skills/return-to-main)
    # or a runner-provisioned copy of it; the config repo's accounts.json is three
    # levels up from the former and absent for the latter (the root rung covers it).
    for aj in (os.path.join(root_native, "qontinui-claude-config", "accounts.json"),
               os.path.join(script_dir, "..", "..", "..", "accounts.json")):
        doc = load(aj)
        for a in ((doc or {}).get("accounts") or []) if isinstance(doc, dict) else []:
            if isinstance(a, dict) and a.get("id"):
                cands.add(os.path.join(acct_root, ".claude-%s" % a["id"]))
    if os.environ.get("CLAUDE_CONFIG_DIR"):
        cands.add(os.environ["CLAUDE_CONFIG_DIR"])
    cands.add(os.path.join(home, ".claude"))
    proj_dirs = sorted({os.path.normcase(os.path.abspath(os.path.join(c, "projects"))) for c in cands
                        if os.path.isdir(os.path.join(c, "projects"))})
    enc = lambda p: re.sub(r"[^A-Za-z0-9]", "-", p.rstrip("/\\"))
    enc_root = enc(root_native)
    newest_by_proj, newest_all = {}, None
    for pd in proj_dirs:
        for dp, dn, fn in os.walk(pd):
            if self_sid and self_sid in dn:
                dn.remove(self_sid)  # this job's own subagent transcripts
            for f in fn:
                if not f.endswith(".jsonl") or (self_sid and f == self_sid + ".jsonl"):
                    continue
                try:
                    m = os.stat(os.path.join(dp, f)).st_mtime
                except OSError:
                    continue
                proj = os.path.relpath(dp, pd).replace("\\", "/").split("/")[0]
                if m > newest_by_proj.get(proj, (0, None))[0]:
                    newest_by_proj[proj] = (m, pd)
                if newest_all is None or m > newest_all[0]:
                    newest_all = (m, proj)
    for repo, v in per_repo.items():
        er = enc(os.path.join(root_native, repo))
        best = None
        for proj, (m, _pd) in newest_by_proj.items():
            # The project-dir name is the session's cwd with every
            # non-alphanumeric turned into '-', so it is lossy: a prefix match
            # over-attributes (`repo-foo` counts for `repo`), which only ever
            # ADDS recent activity. A session at the workspace root, or above
            # it, can touch any repo, so it counts for every one.
            if proj == er or proj.startswith(er + "-") or proj == enc_root or enc_root.startswith(proj + "-"):
                if best is None or m > best:
                    best = m
        v["newest_transcript_write"] = iso(best)
        v["newest_transcript_age_s"] = int(NOW - best) if best is not None else None
    if not proj_dirs:
        probe("transcripts", "unknown", "no Claude account projects/ directory found (accounts root %s)" % acct_root)
    else:
        probe("transcripts", "ok", "%d projects dir(s); newest write %s%s" % (
            len(proj_dirs), iso(newest_all[0]) if newest_all else "none",
            " (this job's own transcript excluded)" if self_sid else " (no --self-session-id: own transcript NOT excluded)"))

    # ---- verdict --------------------------------------------------------------
    unknown = any(p["status"] == "unknown" for p in probes)
    if blocking:
        verdict = "BUSY"
    elif unknown:
        verdict = "UNKNOWN"
    elif overridable:
        verdict = "BUSY"
    else:
        verdict = "QUIET"
    doc = {
        "verdict": verdict,
        "override_eligible": verdict == "BUSY" and not blocking,
        "blocking": blocking,
        "overridable": overridable,
        "ide_checks_ignored": ide_ignored,
        "per_repo": per_repo,
        "newest_transcript_write": ({"at": iso(newest_all[0]), "age_s": int(NOW - newest_all[0]), "project": newest_all[1]}
                                    if newest_all else None),
        "self": {"pids": sorted(self_pids), "agent_pids": sorted(self_agents), "resolved": self_resolved,
                 "session_id": self_sid or None},
        "probes": probes,
        "as_of": iso(NOW),
        "root": root,
    }
    if mode == "json":
        print(json.dumps(doc))
    else:
        print("MACHINE QUIESCE CHECK  %s  root=%s" % (doc["as_of"], root))
        print("  verdict: %s%s" % (verdict, "  (overridable only -- D3's per-repo override may apply)" if doc["override_eligible"] else ""))
        for title, arr in (("blocking", blocking), ("overridable", overridable),
                           ("ignored rust-analyzer checks", ide_ignored)):
            print("  %s: %d" % (title, len(arr)))
            for e in arr:
                print("    - " + ", ".join("%s=%s" % (k, e[k]) for k in e if e[k] not in (None, [], "")))
        print("  probes:")
        for p in probes:
            print("    %-24s %-15s %s" % (p["name"], p["status"], p["detail"]))
        print("  per repo (custody age / transcript age):")
        for r, v in per_repo.items():
            fmt = lambda s: "-" if s is None else ("%dh%02dm" % (s // 3600, (s % 3600) // 60))
            print("    %-36s %-9s %s" % (r, fmt(v["last_custody_age_s"]), fmt(v["newest_transcript_age_s"])))
    return {"QUIET": 0, "BUSY": 1, "UNKNOWN": 3}[verdict]


if __name__ == "__main__":
    sub = sys.argv[1]
    if sub == "ports":
        cmd_ports(sys.argv[2])
    elif sub == "device-id":
        cmd_device_id(sys.argv[2])
    elif sub == "proxy":
        cmd_proxy(sys.argv[2], sys.argv[3])
    elif sub == "cred":
        cmd_cred(sys.argv[2], sys.argv[3])
    elif sub == "next-cursor":
        cmd_next_cursor(sys.argv[2])
    elif sub == "final":
        sys.exit(cmd_final(*sys.argv[2:11]))
    else:
        sys.exit(4)
PYEOF
QPY="$(native "$WORK/qmqc.py")"
qpy() { "${PY[@]}" "$QPY" "$@"; }

# ---- 1. census -------------------------------------------------------------
# The census resolves its own interpreter with the unbounded loop this script
# used to run, so hand it the one proven above. Only a PATH crosses a process
# boundary, never the shim's python3 function. `$PYTHON` carries one word, so a
# launcher that needs its `-3` gets a one-line wrapper that supplies it.
CENSUS_PY="$HOOK_PY3_EXE"
if [ -n "${HOOK_PY3_ARG:-}" ]; then
  CENSUS_PY="$WORK/census-python"
  printf '#!/bin/sh\nexec %s %s "$@"\n' "$(printf '%q' "$HOOK_PY3_EXE")" "$(printf '%q' "$HOOK_PY3_ARG")" > "$CENSUS_PY"
  chmod +x "$CENSUS_PY"
fi
PYTHON="$CENSUS_PY" bash "$CENSUS" --json >"$WORK/census.json" 2>"$WORK/census.err"; CENSUS_RC=$?

# ---- 2. runner instances + readiness ----------------------------------------
http_fetch sup_runners GET "http://127.0.0.1:9875/runners" "" "" 10
http_fetch sup_builds GET "http://127.0.0.1:9875/builds" "" "" 10
for port in $(qpy ports "$(native "$WORK")"); do
  http_fetch "readiness_$port" GET "http://127.0.0.1:$port/restart-readiness" "" "" "$READINESS_TIMEOUT"
done

# ---- 3. coord session rows ----------------------------------------------------
DEVICE_ID="$(qpy device-id "$(native "$HOME")")"
MAX_PAGES=10
fetch_fleet_pages() { # <prefix> <base> <header-file>
  local prefix="$1" base="$2" hdr="$3" page=1 cursor="" q
  while [ "$page" -le "$MAX_PAGES" ]; do
    q="device_id=$DEVICE_ID&limit=200"; [ -n "$cursor" ] && q="$q&cursor=$cursor"
    http_fetch "${prefix}$page" GET "$base/coord/sessions/fleet?$q" "$hdr"
    case "$(http_code "${prefix}$page")" in 2*) ;; *) return 1 ;; esac
    cursor="$(qpy next-cursor "$(native "$WORK/http/${prefix}$page.body")")"
    [ -n "$cursor" ] || return 0
    page=$((page + 1))
  done
  return 0
}
if [ -n "$DEVICE_ID" ]; then
  PROXY_OK=0
  if [ -f "$ROOT/.mcp.json" ]; then
    PROXY_BASE="$(qpy proxy "$(native "$ROOT/.mcp.json")" "$(native "$WORK/proxy.hdr")")"
    if [ -n "$PROXY_BASE" ] && [ -s "$WORK/proxy.hdr" ]; then
      fetch_fleet_pages coord_proxy_fleet_ "$PROXY_BASE" "$WORK/proxy.hdr" && PROXY_OK=1
    fi
    rm -f "$WORK/proxy.hdr"
  fi
  if [ "$PROXY_OK" = 0 ]; then
    http_fetch coord_credential POST "$COORD/agents/credential" "" "{\"device_id\":\"$DEVICE_ID\"}"
    if [ "$(qpy cred "$(native "$WORK/http/coord_credential.body")" "$(native "$WORK/bearer.hdr")")" = ok ]; then
      rm -f "$WORK/http/coord_credential.body"
      fetch_fleet_pages coord_fleet_ "$COORD" "$WORK/bearer.hdr"
    fi
    rm -f "$WORK/bearer.hdr" "$WORK/http/coord_credential.body"
  fi
fi

# ---- 4. aggregate --------------------------------------------------------------
MODE=text; [ "$JSON" = 1 ] && MODE=json
qpy final "$(native "$WORK")" "$(native "$WORK/census.json")" "$CENSUS_RC" "$ROOT" "$(native "$ROOT")" \
  "$SELF_SID" "$DEVICE_ID" "$(native "$SCRIPT_DIR")" "$MODE"
exit $?
