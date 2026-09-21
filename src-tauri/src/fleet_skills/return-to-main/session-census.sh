#!/usr/bin/env bash
# session-census.sh - who is live on THIS box, and what would a runner restart kill?
#
# Linux, macOS and Windows (from Git Bash). Plan
# 2026-09-13-nightly-return-to-main-sweep, Phase 2a; the Linux-only original
# landed with plan 2026-09-05.
#
# WHY THIS DOES NOT ASK THE RUNNER.
# The runner publishes `data.sessionTracking` on :9876/health, and the obvious
# thing is to read it. Two reasons not to, both measured on merytshost
# 2026-09-05:
#
#   1. It is the thing you are about to restart. A restart-safety check served
#      BY the restart target is circular: if it is wedged, degraded, or mid-
#      rebuild, the number you are trusting is produced by the component whose
#      health is in question.
#   2. It disagreed with the process table. It read
#      `liveClaudeTotal: 0, trackedOpenTotal: 0, liveUntracked: 0` while 2
#      runner-spawned sessions and 132 tmux-hosted sessions were live.
#
# And a third, measured on nomad 2026-09-13: the runner's census counts `claude`
# processes in ITS OWN process subtree only, so 3 interactive sessions
# (`claude.exe <- powershell.exe <- WindowsTerminal.exe`) were invisible to it.
# The process table cannot be stale and cannot be wedged. It is ground truth.
#
# THE SOURCE, PER OS (one normalized table, one classifier):
#   linux    ps -eo pid=,ppid=,etime=,args=      (+ /proc/<pid>/environ, cwd)
#   macos    ps -axo pid=,ppid=,etime=,args=
#   windows  powershell -NoProfile, ONE pass emitting
#            {Processes, Services, ServicesError}:
#            Get-CimInstance Win32_Process -> ProcessId, ParentProcessId, Name,
#            CreationDate, SessionId, CommandLine (CommandLine is read only for
#            rostered images; it is used to MATCH and is never printed, so no
#            argv secret reaches the output); Get-CimInstance Win32_Service ->
#            Name, ProcessId, State for every service with a process. A failed
#            service read is recorded in ServicesError, not fatal: it only
#            withholds the service proof below. A bare Win32_Process array (an
#            older collector or fixture) parses, with no service table.
#
# THE ROSTER: `claude` (native binary, or `node` running the Claude Code CLI),
# `codex`, `pi` (agents); `qontinui-runner` (instances); `cargo`, `rustc`,
# `clippy-driver` (the build signal that needs no dev-only tooling -- plan D3).
#
# ORIGIN of each agent process:
#   runner    a `qontinui-runner` in its ancestry, or (Linux) a non-empty
#             $QONTINUI_RUNNER_CONTEXT in its environment. A runner restart
#             KILLS these.
#   external  anything else -- a terminal, an IDE, tmux. A runner restart does
#             not touch them, and the runner cannot see them.
#   self      the NEAREST agent on THIS census process's own ancestry, plus
#             everything descended from that agent. Nothing further up the
#             chain is self: when the checking session is a `claude` launched
#             from another live session's Bash tool, the OUTER session, its
#             other children and its builds are counted like anyone else's.
#             A runner ends the walk below it -- a runner is never self.
#             Measured 2026-09-13 (plan Phase 0.2): a scheduled session ran as
#             `claude.exe <- powershell.exe <- (exited)`, with NO runner in its
#             ancestry -- so runner ancestry cannot recognise the job, and the
#             job must never count itself. So self is found by walking the
#             checking process's OWN ancestry, never by runner ancestry.
#             On Windows the walk starts from both this bash's Windows pid
#             (/proc/$$/winpid; MSYS $$ is not a Windows pid) and the
#             PowerShell collector's own $PID, and a parent whose creation time
#             is AFTER its child's is a reused pid and ends the walk. If two
#             starts reach different agents, the DEEPEST is self (the one the
#             other is an ancestor of), so self can only shrink.
#
# AN UNREADABLE SIGNAL IS NOT AN IDLE ONE. Windows returns a null CommandLine
# for a process the caller may not query (an elevated one, another user's), and
# a `node` is only an agent by its command line -- so such a node can be neither
# classified nor ruled out. It is reported in `unreadable_nodes` (count + pids),
# never silently dropped, and machine-quiesce-check.sh reads it as UNKNOWN.
#
# ONE EXCEPTION, A PROVEN WINDOWS SERVICE -- proven by an ALLOWLIST of shapes.
# A node service running as SYSTEM is the common reason a command line is
# unreadable at all, and counting it would make every such box UNKNOWN every
# night with no remedy: the D3 "not applicable is not UNKNOWN" failure. But
# session 0 ALONE proves nothing, and neither does "reaches services.exe with no
# shell on the way": the set of session-0 launchers that start processes for a
# human or on a schedule is open-ended -- OpenSSH (sshd), WinRM (wsmprovhost),
# remote WMI (WmiPrvSE), DCOM (dllhost), PsExec (PSEXESVC), Task Scheduler
# (svchost), and any third-party remote-management service. A refusal list of
# them could never be complete: measured 2026-09-13, the wsmprovhost, WmiPrvSE,
# PSEXESVC and `services <- svchost <- node` chains all read QUIET under one.
# So an unreadable node is ignored ONLY when its ancestry matches one of two
# shapes (ancestor on the left), walked with the reused-pid guard:
#   (a) services.exe <- node.exe [<- node.exe]*
#       the topmost node's pid is the ProcessId of exactly ONE registered
#       service (the same shared-host refusal as W below);
#   (b) services.exe <- W <- node.exe [<- node.exe]*
#       W (a service wrapper: WinSW, nssm, node-windows' daemon) sits DIRECTLY
#       under services.exe, W's pid is the ProcessId of exactly ONE registered
#       service (a process hosting several is a shared host), and W's image is
#       none of the multi-purpose / remote-execution hosts -- svchost, dllhost,
#       WmiPrvSE, wsmprovhost, PSEXESVC -- nor, as a second layer, a logon,
#       task, shell or terminal host: sshd*, ssh-shellhost, taskhostw, taskeng,
#       cmd, powershell, pwsh, bash, sh, wsl, conhost, OpenConsole,
#       WindowsTerminal.
# The node-to-node hops keep node-windows / pm2 daemons (wrapper -> node ->
# node) passing. "Registered" is read from Win32_Service in the SAME PowerShell
# pass as Win32_Process (readable without elevation: measured 2026-09-13 on
# nomad, non-admin, 133 services with a ProcessId). ANYTHING ELSE IS NOT PROOF
# and the node stays in `unreadable_nodes` (UNKNOWN): svchost anywhere, a second
# non-node intermediate, a runner (`qontinui-runner*`) or agent ancestor, a
# chain that cannot be walked (a parent missing from the table, a reused pid, a
# loop), a missing / null / non-zero SessionId on the node or any element of the
# proof, a null CreationDate on any element of the proof (the reused-pid guard
# cannot run without it), or a Win32_Service query that failed. A
# runner-descended node never gets this far: it takes the runner path, so the
# exit-3 promise below holds even for a runner installed as a service. An
# ignored node is reported under `service_nodes_ignored` with the
# `service_chain` and the `service_name` that justified it, never silently
# dropped; a session-0 node that failed the proof stays in `unreadable_nodes`
# carrying `not_a_service` (the reason). Linux and macOS have no such field and
# are unaffected. Only `node` needs this: a `claude`/`codex`/`pi` image is an
# agent by its name alone, readable or not.
# RESIDUAL, stated rather than hidden, and it is wider than "remote execution":
# ANY registered single-purpose service directly under services.exe and off the
# refusal list whose process IS node.exe (shape a) or STARTS node.exe (shape b)
# reads as a service, together with every node descended from it through the
# node-only hops. That includes agent-hosting services and remote-dev servers --
# e.g. code-server, a node app registered as a service in its own right, or an
# Agent SDK app installed with nssm as LocalSystem running Claude Code as
# `node cli.js` under its own node (services <- nssm <- node <- node) -- which
# then read as QUIET. Nothing distinguishes these from an ordinary node service
# without reading the command line, which needs elevation; every such setup
# needs an administrator to register the service, and without the carve-out a
# box running any node service as SYSTEM (command line unreadable) would be
# UNKNOWN every night. (Measured 2026-09-13 on nomad: CoworkVMService --
# cowork-svc.exe, LocalSystem, the only service on its pid -- fills the W
# position of shape (b) but starts no node, so it is not an active failure
# there.)
# DELIBERATE TRADE: a node service wrapped by a `.bat` / `cmd` (services <-
# wrapper <- cmd <- node) is not a provable shape and reads UNKNOWN every night;
# re-register it with the wrapper starting node.exe directly.
#
# AN IDE'S OWN CHECK IS NOT A BUILD. rust-analyzer's flycheck runs cargo's
# check subcommand whenever an editor has a Rust workspace open, so counting it
# would keep every box with an IDE left open overnight BUSY forever (plan
# 2026-09-13-heartbeat-stop-python3-stub-ide-cargo-quiesce, D2). A build row
# is an IDE check iff its NEAREST ancestor that is not itself part of a build
# (skipping cargo, rustc, clippy-driver, build-script-*, and the hops a build
# passes through without being one: the rustup toolchain proxy, an sccache
# RUSTC_WRAPPER and cargo-clippy) is named `rust-analyzer`. An element of the
# walk, the row itself included, with no creation time ends it as NOT an IDE
# check, because the reused-pid guard cannot run on it. Measured 2026-09-14 on
# spaceship (Windows 11), rust-analyzer 1.95.0 driven headless over LSP: the
# flycheck ran as `cargo.exe <- rustup.exe <- rust-analyzer.exe`, because
# ~/.cargo/bin/cargo.exe IS rustup.exe -- so without the rustup hop a real
# IDE check would still read as a build.
# The key is ancestry, not argv: rust-analyzer.check.overrideCommand makes the
# command arbitrary. `code` / `cursor` as the nearest launcher is NOT exempt --
# that is a person's `cargo build` in an IDE terminal (cargo <- bash <- code).
# Such rows carry `ide_check: true`, are left out of `builds`, and are reported
# under `ide_checks_ignored`, never dropped. RESIDUAL, stated: an
# overrideCommand that wraps cargo in a shell (rust-analyzer <- sh <- cargo)
# reads as a real build -- BUSY, the safe side. So does every other IDE's own
# check (RustRover / IntelliJ run cargo themselves): only rust-analyzer is
# recognised, so such an IDE left open still reads BUSY. Not yet measured: a VS Code or
# Cursor extension host launching its bundled rust-analyzer (the chain BELOW
# rust-analyzer is what decides, and that is what was measured), and Linux.
#
# WHAT A RUNNER RESTART ACTUALLY KILLS: the `runner`-origin sessions. Sessions
# you started yourself survive it. They die only to a blanket `pkill node` /
# `pkill claude`, which is separately forbidden [policy: production-and-cost
# runner-lifecycle].
#
# Usage:  session-census.sh [--json] [--quiet] [--self-pid PID[,PID...]]
#   --json       one JSON object on stdout (the contract machine-quiesce-check.sh
#                reads): {as_of, host, os, source, self:{resolved, starts, pids,
#                agent_pids}, total, runner_spawned, external, restart_kills,
#                self_agents, builds, runners, processes:[{pid, ppid, kind,
#                name, origin, age_s, nested_under_agent, launched_by,
#                ide_check (true | false on a build row, null otherwise),
#                ancestry, account, cwd, idle_min}], unreadable_nodes:{count,
#                pids, processes:[{pid, ppid, name, age_s, win_session,
#                runner_descended, ancestry, not_a_service?}]},
#                service_nodes_ignored:{count, pids, processes:[same shape, with
#                service_chain, service_name and service_shape (a | b) instead
#                of not_a_service]}, ide_checks_ignored:{count, pids,
#                processes:[{pid, ppid, name, age_s, launched_by, ancestry}]},
#                service_table:{status (ok | unreadable |
#                not_applicable), count, error}}. `total`/`external`/`runner_spawned` EXCLUDE
#                self; `builds` excludes self and IDE checks; `self.pids` is the walked chain up to the nearest agent
#                plus every descendant of that agent.
#   --quiet      print nothing; the exit code is the answer.
#   --self-pid   start the self walk here instead of at this process.
#
# Fixture inputs (tests; no live system needed):
#   SESSION_CENSUS_OS        linux | macos | windows   (default: uname)
#   SESSION_CENSUS_PS_FILE   canned process table in that OS's source format
#   SESSION_CENSUS_SELF_PID  self-walk start pid(s), in the table's pid space
#   SESSION_CENSUS_PROC      a /proc root for the Linux environ/cwd reads
#   SESSION_CENSUS_NOW       epoch seconds used as "now"
#   PYTHON                   interpreter to try first
#
# Exit:   0 = no runner-hosted agent session other than self
#         1 = runner-hosted agent sessions are live (a restart kills them)
#         2 = the census itself could not run
#         3 = none confirmed, but a runner-descended `node` has an unreadable
#             command line and may be one -- UNKNOWN, never 0
set -u

JSON=0; QUIET=0; SELF_PID_ARG=""
while [ $# -gt 0 ]; do
  case "$1" in
    --json)  JSON=1 ;;
    --quiet) QUIET=1 ;;
    --self-pid)
      shift; [ $# -gt 0 ] || { echo "session-census: --self-pid needs a pid" >&2; exit 2; }
      SELF_PID_ARG="$1" ;;
    -h|--help) sed -n '2,/^set -u$/{/^set -u$/d;p;}' "$0"; exit 0 ;;
    *) echo "session-census: unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

# A Windows `python3` is usually the Microsoft Store stub, which exits non-zero
# without running anything -- so every candidate is RUN, not just located.
resolve_python() {
  local c
  for c in "${PYTHON:-}" python3 python; do
    [ -n "$c" ] || continue
    command -v "$c" >/dev/null 2>&1 || continue
    "$c" -c 'import json, sys; sys.exit(0 if sys.version_info >= (3, 6) else 1)' >/dev/null 2>&1 \
      && { printf '%s' "$c"; return 0; }
  done
  return 1
}

# A native interpreter cannot open an MSYS path such as /tmp/x; `cygpath -m`
# renders C:/... . Elsewhere it is the identity.
native() {
  if command -v cygpath >/dev/null 2>&1; then cygpath -m "$1"; else printf '%s' "$1"; fi
}

PY="$(resolve_python)" || { echo "session-census: no working python3/python interpreter" >&2; exit 2; }

OS="${SESSION_CENSUS_OS:-}"
if [ -z "$OS" ]; then
  case "$(uname -s 2>/dev/null)" in
    Linux*) OS=linux ;;
    Darwin*) OS=macos ;;
    MINGW*|MSYS*|CYGWIN*) OS=windows ;;
    *) OS=unknown ;;
  esac
fi

WORK="$(mktemp -d 2>/dev/null)" || { echo "session-census: mktemp failed" >&2; exit 2; }
trap 'rm -rf "$WORK"' EXIT
RAW="$WORK/ps.raw"
SELF_STARTS="${SELF_PID_ARG:-${SESSION_CENSUS_SELF_PID:-}}"
SOURCE=""

if [ -n "${SESSION_CENSUS_PS_FILE:-}" ]; then
  cp "$SESSION_CENSUS_PS_FILE" "$RAW" 2>/dev/null || { echo "session-census: cannot read $SESSION_CENSUS_PS_FILE" >&2; exit 2; }
  SOURCE="fixture:$OS"
else
  case "$OS" in
    linux)
      ps -eo pid=,ppid=,etime=,args= >"$RAW" 2>"$WORK/ps.err" || { echo "session-census: ps failed: $(head -c 300 "$WORK/ps.err")" >&2; exit 2; }
      SOURCE="ps -eo pid=,ppid=,etime=,args= + /proc"
      [ -n "$SELF_STARTS" ] || SELF_STARTS="$$" ;;
    macos)
      ps -axo pid=,ppid=,etime=,args= >"$RAW" 2>"$WORK/ps.err" || { echo "session-census: ps failed: $(head -c 300 "$WORK/ps.err")" >&2; exit 2; }
      SOURCE="ps -axo pid=,ppid=,etime=,args="
      [ -n "$SELF_STARTS" ] || SELF_STARTS="$$" ;;
    windows)
      PS_BIN=""
      for c in powershell.exe powershell pwsh.exe pwsh; do
        command -v "$c" >/dev/null 2>&1 && { PS_BIN="$c"; break; }
      done
      [ -n "$PS_BIN" ] || { echo "session-census: no powershell on this Windows host" >&2; exit 2; }
      # Only single quotes inside: Windows PowerShell 5.1 mangles an embedded
      # double quote on a native command line. SessionId is passed through
      # UNCAST on purpose: [int]$null is 0, which would turn an unreadable
      # session into session 0 -- the one value that excuses an unreadable node.
      # The Win32_Service read is try/caught on its own: its failure withholds
      # the service proof (ServicesError) instead of failing the whole census.
      # -Depth 4: the object nests one level deeper than the old bare array;
      # measured 2026-09-13 (PS 5.1), CreationDate still serializes as \/Date()\/.
      PS_CMD='$ErrorActionPreference='"'"'Stop'"'"'; [Console]::OutputEncoding=[Text.Encoding]::UTF8; if ($env:QSC_SELF_FILE) { [IO.File]::WriteAllText($env:QSC_SELF_FILE, [string]$PID) }; $r='"'"'^(claude|node|codex|pi|qontinui-runner|cargo|rustc|clippy-driver)(\.exe)?$'"'"'; $p=@(Get-CimInstance Win32_Process | ForEach-Object { $cl = $null; if ($_.Name -match $r) { $cl = $_.CommandLine }; [pscustomobject]@{ProcessId=[int]$_.ProcessId; ParentProcessId=[int]$_.ParentProcessId; Name=$_.Name; CreationDate=$_.CreationDate; SessionId=$_.SessionId; CommandLine=$cl} }); $s=$null; $se=$null; try { $s=@(Get-CimInstance Win32_Service | Where-Object { $_.ProcessId } | ForEach-Object { [pscustomobject]@{Name=$_.Name; ProcessId=[int]$_.ProcessId; State=$_.State} }) } catch { $s=$null; $se=[string]$_.Exception.Message }; [pscustomobject]@{Processes=$p; Services=$s; ServicesError=$se} | ConvertTo-Json -Compress -Depth 4'
      QSC_SELF_FILE="$(native "$WORK/ps.self")" "$PS_BIN" -NoProfile -NonInteractive -Command "$PS_CMD" \
        >"$RAW" 2>"$WORK/ps.err" || { echo "session-census: Win32_Process read failed: $(head -c 300 "$WORK/ps.err")" >&2; exit 2; }
      SOURCE="powershell Get-CimInstance Win32_Process + Win32_Service"
      if [ -z "$SELF_STARTS" ]; then
        # The Windows parent chain BREAKS at MSYS exec boundaries: measured on
        # nomad 2026-09-13, a `bash script.sh`'s Windows parent had already
        # exited, so a Win32-only walk from /proc/$$/winpid stopped two hops
        # up and never reached the session's claude.exe. MSYS keeps its own
        # parent links, so every winpid on the MSYS chain is a start; the
        # topmost one (MSYS ppid 1) has a live Win32 parent, which carries the
        # walk on to the native launcher.
        p=$$; i=0
        while [ "$i" -lt 64 ] && [ -r "/proc/$p/winpid" ]; do
          w="$(cat "/proc/$p/winpid" 2>/dev/null)"
          [ -n "$w" ] && SELF_STARTS="${SELF_STARTS:+$SELF_STARTS,}$w"
          pp="$(cat "/proc/$p/ppid" 2>/dev/null)"
          case "$pp" in ''|0|1|*[!0-9]*) break ;; esac
          [ "$pp" = "$p" ] && break
          p="$pp"; i=$((i + 1))
        done
        [ -s "$WORK/ps.self" ] && SELF_STARTS="${SELF_STARTS:+$SELF_STARTS,}$(tr -dc '0-9' <"$WORK/ps.self")"
      fi ;;
    *) echo "session-census: unsupported OS '$(uname -s 2>/dev/null)'" >&2; exit 2 ;;
  esac
fi

MODE=text; [ "$JSON" = 1 ] && MODE=json; [ "$QUIET" = 1 ] && [ "$JSON" = 0 ] && MODE=quiet

cat >"$WORK/census.py" <<'PYEOF'
import json, os, re, socket, sys, time
from datetime import datetime, timezone

os_name, raw_path, self_starts, proc_root, mode, source = sys.argv[1:7]
now = float(os.environ.get("SESSION_CENSUS_NOW") or time.time())

AGENTS = {"claude", "codex", "pi"}
RUNNER = "qontinui-runner"
BUILDS = {"cargo", "rustc", "clippy-driver"}
# Processes that sit INSIDE a build's ancestry without being a build row: the
# rustup toolchain proxy (~/.cargo/bin/cargo.exe is rustup.exe on Windows), a
# RUSTC_WRAPPER, and cargo's clippy subcommand (rust-analyzer.check.command =
# "clippy"). Transparent to the IDE-check walk only.
BUILD_HOPS = {"rustup", "sccache", "cargo-clippy"}
# `node` running an agent CLI: matched on the (slash-normalized) command line.
NODE_MARKERS = (
    ("claude", ("@anthropic-ai/claude-code", "/claude-code/cli")),
    ("codex", ("@openai/codex",)),
    ("pi", ("pi-coding-agent",)),
)


def norm_name(n):
    n = (n or "").strip().lower()
    if n.startswith("-"):
        n = n[1:]
    return n[:-4] if n.endswith(".exe") else n


def parse_etime(s):
    # [[dd-]hh:]mm:ss
    m = re.match(r"^(?:(\d+)-)?(?:(\d+):)?(\d+):(\d+)$", s.strip())
    if not m:
        return None
    d, h, mi, se = (int(x) if x else 0 for x in m.groups())
    return d * 86400 + h * 3600 + mi * 60 + se


def parse_created(v):
    if v is None:
        return None
    if isinstance(v, (int, float)):
        return v / 1000.0 if v > 1e11 else float(v)
    s = str(v).strip()
    m = re.match(r"^/Date\((-?\d+)(?:[+-]\d+)?\)/$", s)
    if m:
        return int(m.group(1)) / 1000.0
    m = re.match(r"^(\d{14})\.(\d{1,6})([+-]\d{3})$", s)  # raw CIM DMTF
    if m:
        base = datetime.strptime(m.group(1), "%Y%m%d%H%M%S").replace(tzinfo=timezone.utc)
        return base.timestamp() + int(m.group(2).ljust(6, "0")) / 1e6 - int(m.group(3)) * 60
    m = re.match(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(?:\.(\d+))?(Z|[+-]\d\d:?\d\d)?$", s)
    if m:
        base = datetime.strptime(m.group(1), "%Y-%m-%dT%H:%M:%S")
        frac = float("0." + m.group(2)) if m.group(2) else 0.0
        tz = m.group(3)
        if tz in (None, ""):
            ts = base.timestamp()  # naive: local time
        else:
            off = 0
            if tz != "Z":
                sign = 1 if tz[0] == "+" else -1
                hh, mm = tz[1:].replace(":", "")[:2], tz[1:].replace(":", "")[2:]
                off = sign * (int(hh) * 3600 + int(mm) * 60)
            ts = base.replace(tzinfo=timezone.utc).timestamp() - off
        return ts + frac
    return None


def first_token(args):
    args = args.strip()
    if not args:
        return "", ""
    parts = args.split(None, 2)
    return parts[0], (parts[1] if len(parts) > 1 else "")


def basename(p):
    p = p.strip('"').replace("\\", "/")
    return p.rsplit("/", 1)[-1]


def classify(name, cmd):
    """Return roster kind or None. `name` is the normalized image name."""
    c = (cmd or "").replace("\\", "/").lower()
    if name in AGENTS:
        return name
    if name == RUNNER or name.startswith(RUNNER):
        return "runner"
    if name in BUILDS:
        return "build"
    if name == "node":
        for kind, marks in NODE_MARKERS:
            if any(mk in c for mk in marks):
                return kind
        _, second = first_token(cmd or "")
        b = norm_name(basename(second))
        b = b[:-3] if b.endswith(".js") else b
        if b in AGENTS:
            return b
    # Claude Code's native installer runs .../claude/versions/<ver> when it is
    # invoked by its resolved path rather than through the `claude` symlink.
    if "/claude/versions/" in c.split(" ", 1)[0]:
        return "claude"
    return None


procs = {}
# The Win32_Service table: pid -> [service names] for every service that has a
# process. None when it could not be read -- and None proves nothing.
svc_by_pid, svc_error, svc_count = None, None, None
try:
    with open(raw_path, "rb") as fh:
        text = fh.read().decode("utf-8", "replace").lstrip("\ufeff")
except OSError as e:
    print("session-census: cannot read the process table: %s" % e, file=sys.stderr)
    sys.exit(2)

if os_name == "windows":
    try:
        rows = json.loads(text) if text.strip() else []
    except ValueError as e:
        print("session-census: Win32_Process JSON unparseable: %s" % e, file=sys.stderr)
        sys.exit(2)
    if isinstance(rows, dict) and "Processes" in rows:
        src = rows
        rows = src.get("Processes")
        if isinstance(rows, dict):
            rows = [rows]
        if not isinstance(rows, list):
            print("session-census: the collector's Processes is not a list", file=sys.stderr)
            sys.exit(2)
        svcs = src.get("Services")
        if isinstance(svcs, dict):
            svcs = [svcs]
        if src.get("ServicesError") or not isinstance(svcs, list):
            svc_error = "Win32_Service query failed: %s" % (str(src.get("ServicesError") or "no Services list")[:200])
        else:
            svc_by_pid, svc_count = {}, 0
            for s in svcs:
                if not isinstance(s, dict):
                    continue
                spid = s.get("ProcessId")
                if isinstance(spid, int) and not isinstance(spid, bool) and spid > 0 and s.get("Name"):
                    svc_by_pid.setdefault(spid, []).append(str(s["Name"]))
                    svc_count += 1
    else:
        # A bare Win32_Process array (or pwsh 7's lone object): no service table.
        svc_error = "the process source carries no Win32_Service table"
        if isinstance(rows, dict):
            rows = [rows]
    for r in rows:
        try:
            pid = int(r.get("ProcessId"))
        except (TypeError, ValueError):
            continue
        try:
            ppid = int(r.get("ParentProcessId") or 0)
        except (TypeError, ValueError):
            ppid = 0
        name = r.get("Name") or ""
        # The Windows session: an integer >= 0, or None when it is missing,
        # null or anything else -- and None is never read as session 0.
        sid = r.get("SessionId")
        win_session = sid if isinstance(sid, int) and not isinstance(sid, bool) and sid >= 0 else None
        # A null CommandLine is "could not be read", not "empty": the collector
        # asks for it on every rostered image, so null means access was denied.
        procs[pid] = {"pid": pid, "ppid": ppid, "name": name, "norm": norm_name(name),
                      "created": parse_created(r.get("CreationDate")),
                      "win_session": win_session,
                      "cmd": r.get("CommandLine") or "",
                      "cmd_unreadable": not str(r.get("CommandLine") or "").strip()}
elif os_name in ("linux", "macos"):
    for line in text.splitlines():
        parts = line.strip().split(None, 3)
        if len(parts) < 3 or not parts[0].isdigit() or not parts[1].isdigit():
            continue
        pid, ppid = int(parts[0]), int(parts[1])
        et = parse_etime(parts[2])
        args = parts[3] if len(parts) > 3 else ""
        tok, _ = first_token(args)
        name = basename(tok)
        if name.startswith("[") and name.endswith("]"):
            name = name[1:-1]
        procs[pid] = {"pid": pid, "ppid": ppid, "name": name, "norm": norm_name(name),
                      "created": (now - et) if et is not None else None, "cmd": args}
else:
    print("session-census: unsupported OS %r" % os_name, file=sys.stderr)
    sys.exit(2)

if not procs:
    print("session-census: the process table was empty -- that is a failed read, not an idle box", file=sys.stderr)
    sys.exit(2)

for p in procs.values():
    p["kind"] = classify(p["norm"], p["cmd"])


def parent_of(p):
    q = procs.get(p["ppid"])
    if q is None or q["pid"] == p["pid"]:
        return None
    # Windows keeps a dead parent's pid in ParentProcessId; a LATER process
    # holding that pid is not the parent.
    if q["created"] is not None and p["created"] is not None and q["created"] > p["created"] + 1:
        return None
    return q


def ancestors(p, limit=64):
    out, seen, cur = [], {p["pid"]}, p
    while len(out) < limit:
        cur = parent_of(cur)
        if cur is None or cur["pid"] in seen:
            break
        seen.add(cur["pid"])
        out.append(cur)
    return out


# ---- self: the NEAREST agent on the checking process's own ancestry --------
def self_walk(start):
    """The chain from `start` up to and including the nearest agent."""
    chain, agent = [], None
    for q in [procs[start]] + ancestors(procs[start]):
        if q["kind"] == "runner":
            break  # a runner is never self, and nothing above it is
        chain.append(q["pid"])
        if q["kind"] in AGENTS:
            agent = q["pid"]
            break  # the NEAREST agent ends the walk
    return chain, agent


starts = [int(x) for x in re.split(r"[,\s]+", self_starts.strip()) if x.isdigit()] if self_starts.strip() else []
chain_pids, nearest = set(), set()
for s in starts:
    if s in procs:
        c, a = self_walk(s)
        chain_pids.update(c)
        if a is not None:
            nearest.add(a)
# Two starts reaching different agents: keep the deepest, so an outer session
# is never self merely because one start's Win32 chain skipped the inner one.
self_agent_pids = {a for a in nearest
                   if not any(a in {x["pid"] for x in ancestors(procs[b])} for b in nearest if b != a)}
chain_pids -= nearest - self_agent_pids
self_pids = set(chain_pids)
for p in procs.values():
    if any(a["pid"] in self_agent_pids for a in ancestors(p)):
        self_pids.add(p["pid"])


def linux_env(pid):
    try:
        with open(os.path.join(proc_root, str(pid), "environ"), "rb") as fh:
            items = fh.read().split(b"\0")
    except OSError:
        return {}
    env = {}
    for it in items:
        k, sep, v = it.partition(b"=")
        if sep:
            env[k.decode("utf-8", "replace")] = v.decode("utf-8", "replace")
    return env


def linux_cwd(pid):
    try:
        return os.readlink(os.path.join(proc_root, str(pid), "cwd"))
    except OSError:
        return None


def linux_idle_min(pid, acct):
    # The session holds its scratchpad open, and the scratchpad path carries
    # the session UUID, which names the transcript.
    fd_dir = os.path.join(proc_root, str(pid), "fd")
    sid = None
    try:
        for fd in os.listdir(fd_dir):
            try:
                tgt = os.readlink(os.path.join(fd_dir, fd))
            except OSError:
                continue
            m = re.search(r"/tmp/claude-\d+/[^/]*/([0-9a-f-]{36})/", tgt)
            if m:
                sid = m.group(1)
                break
    except OSError:
        return None
    if not sid or not acct or not os.path.isdir(os.path.join(acct, "projects")):
        return None
    for dp, _dn, fn in os.walk(os.path.join(acct, "projects")):
        if sid + ".jsonl" in fn:
            try:
                return int((now - os.stat(os.path.join(dp, sid + ".jsonl")).st_mtime) // 60)
            except OSError:
                return None
    return None


rows = []
for p in sorted(procs.values(), key=lambda x: x["pid"]):
    kind = p["kind"]
    if kind is None:
        continue
    anc = ancestors(p)
    env =linux_env(p["pid"]) if os_name == "linux" and kind in AGENTS else {}
    in_self_tree = p["pid"] in self_pids
    runner_anc = any(a["kind"] == "runner" for a in anc)
    if kind in AGENTS or kind == "build":
        if in_self_tree:
            origin = "self"
        elif runner_anc or env.get("QONTINUI_RUNNER_CONTEXT"):
            origin = "runner"
        else:
            origin = "external"
    else:
        origin = "self" if in_self_tree else ("runner" if runner_anc else "external")
    nested = any(a["kind"] in AGENTS for a in anc)
    # An IDE's own check (see the header): the NEAREST ancestor that is not
    # itself part of a build decides, so a rustup proxy, an sccache wrapper or
    # a build script between rust-analyzer and this process does not hide it,
    # while a shell (a person's terminal, or an overrideCommand wrapper) does.
    ide_check = None
    if kind == "build":
        ide_check = False
        # An undated row, like an undated ancestor below, leaves the reused-pid
        # guard blind on its first edge: not proof of an IDE.
        for a in (anc if p["created"] is not None else []):
            if a["created"] is None:
                break  # parent_of() cannot rule out a reused pid here: not proof of an IDE
            if a["kind"] == "build" or a["norm"] in BUILD_HOPS or a["norm"].startswith("build-script-"):
                continue
            ide_check = a["norm"] == "rust-analyzer"
            break  # the NEAREST non-build ancestor decides (D2)
    launched_by = None
    for a in anc:
        if a["kind"] in AGENTS or a["kind"] == "runner" or a["norm"] in ("rust-analyzer", "code", "cursor"):
            launched_by = "%s(%d)" % (a["name"], a["pid"])
            break
    acct = env.get("CLAUDE_CONFIG_DIR") if env else None
    rows.append({
        "pid": p["pid"],
        "ppid": p["ppid"],
        "kind": kind,
        "name": p["name"],
        "origin": origin,
        "age_s": int(now - p["created"]) if p["created"] is not None and p["created"] <= now + 1 else None,
        "nested_under_agent": nested,
        "launched_by": launched_by,
        "ide_check": ide_check,
        "ancestry": ["%s(%d)" % (a["name"], a["pid"]) for a in anc[:8]],
        "account": os.path.basename(acct.rstrip("/")) if acct else None,
        "cwd": linux_cwd(p["pid"]) if os_name == "linux" and kind in AGENTS else None,
        "idle_min": linux_idle_min(p["pid"], acct) if os_name == "linux" and kind in AGENTS else None,
    })

# A `node` is an agent only by its command line; one whose command line could
# not be read can be neither classified nor ruled out. Inside self it is self's
# whatever it is; anywhere else it is reported, never dropped. The one carve-out
# is a PROVEN Windows service (see the header), proven by an ALLOWLIST of
# ancestry shapes -- never by the absence of a known launcher, because the set
# of session-0 launchers is open-ended:
#   (a) services.exe <- node [<- node]*       the top node is ONE registered service
#   (b) services.exe <- W <- node [<- node]*  W is ONE registered service, not a
#                                             host on either list below
# A missing session (win_session None) is NOT session 0.
REMOTE_EXEC_HOSTS = {"svchost", "dllhost", "wmiprvse", "wsmprovhost", "psexesvc"}
NOT_A_SERVICE = {"ssh-shellhost", "taskhostw", "taskeng", "cmd", "powershell", "pwsh",
                 "bash", "sh", "wsl", "conhost", "openconsole", "windowsterminal"}


def label(q):
    return "%s(%d)" % (q["name"], q["pid"])


def reuse_guard_blind(q):
    """parent_of() cannot tell a reused pid without a CreationDate."""
    return q["created"] is None


def proof_refusal(q):
    """Why q cannot be an element of a service proof, or None."""
    if q["kind"] == "runner":
        return "%s is a runner: a runner-descended node takes the runner path" % label(q)
    if q["kind"] in AGENTS:
        return "%s is an agent: a node under an agent is not a service" % label(q)
    if q["norm"] in REMOTE_EXEC_HOSTS:
        return "%s is a multi-purpose or remote-execution host" % label(q)
    if q["norm"] in NOT_A_SERVICE or q["norm"].startswith("sshd"):
        return "%s is a logon, task, shell or terminal host" % label(q)
    if reuse_guard_blind(q):
        return "%s has no CreationDate, so the reused-pid guard cannot run" % label(q)
    if q.get("win_session") != 0:
        return "%s is not in Windows session 0" % label(q)
    return None


def service_proof(p):
    """({chain, service_name, shape}, None) when p's ancestry matches shape (a)
    or (b); (None, reason) for every other shape."""
    if svc_by_pid is None:
        return None, svc_error or "no Win32_Service table"
    if reuse_guard_blind(p):
        return None, "%s has no CreationDate, so the reused-pid guard cannot run" % label(p)
    chain, seen, cur, top = [], {p["pid"]}, p, p
    while True:
        if len(chain) >= 64:
            return None, "ancestry deeper than 64 without reaching services.exe"
        q = parent_of(cur)
        if q is None or q["pid"] in seen:
            return None, "ancestry breaks above %s before reaching services.exe" % label(cur)
        seen.add(q["pid"])
        chain.append(label(q))
        why = proof_refusal(q)
        if why:
            return None, why
        if q["norm"] != "node":
            break
        cur = top = q  # a node-to-node hop (a pm2 / node-windows daemon)
    if q["norm"] == "services":
        host, shape = top, "a"
    else:
        host, shape = q, "b"
        parent = parent_of(q)
        if parent is None or parent["pid"] in seen:
            return None, "ancestry breaks above %s before reaching services.exe" % label(q)
        if parent["norm"] != "services":
            return None, "%s is not directly under services.exe (its parent is %s): a second non-node intermediate proves no service" % (
                label(q), label(parent))
        why = proof_refusal(parent)
        if why:
            return None, why
        chain.append(label(parent))
    names = sorted(svc_by_pid.get(host["pid"]) or [])
    if not names:
        return None, "%s is not the process of any registered service (Win32_Service)" % label(host)
    if len(names) > 1:
        return None, "%s hosts %d registered services (%s): a shared host is not single-purpose" % (
            label(host), len(names), ", ".join(names[:4]))
    return {"chain": chain, "service_name": ", ".join(names) or None, "shape": shape}, None


unreadable, service_nodes = [], []
for p in sorted(procs.values(), key=lambda x: x["pid"]):
    if p["kind"] is None and p["norm"] == "node" and p.get("cmd_unreadable") and p["pid"] not in self_pids:
        anc = ancestors(p)
        ent = {
            "pid": p["pid"],
            "ppid": p["ppid"],
            "name": p["name"],
            "age_s": int(now - p["created"]) if p["created"] is not None and p["created"] <= now + 1 else None,
            "win_session": p.get("win_session"),
            "runner_descended": any(a["kind"] == "runner" for a in anc),
            "ancestry": ["%s(%d)" % (a["name"], a["pid"]) for a in anc[:8]],
        }
        proof, refusal = (service_proof(p) if os_name == "windows" and p.get("win_session") == 0
                          else (None, None))
        if proof is not None:
            ent["service_chain"] = proof["chain"]
            ent["service_name"] = proof["service_name"]
            ent["service_shape"] = proof["shape"]
            service_nodes.append(ent)
        else:
            if refusal:
                ent["not_a_service"] = refusal
            unreadable.append(ent)

agents = [r for r in rows if r["kind"] in AGENTS]
runner_n =sum(1 for r in agents if r["origin"] == "runner")
ext_n = sum(1 for r in agents if r["origin"] == "external")
self_n = sum(1 for r in agents if r["origin"] == "self")
ide_checks = [r for r in rows if r["kind"] == "build" and r["origin"] != "self" and r["ide_check"]]
builds_n = sum(1 for r in rows if r["kind"] == "build" and r["origin"] != "self" and not r["ide_check"])
runners_n = sum(1 for r in rows if r["kind"] == "runner")

doc = {
    "as_of": datetime.fromtimestamp(now, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "host": socket.gethostname(),
    "os": os_name,
    "source": source,
    "self": {"resolved": bool(self_pids), "starts": starts, "pids": sorted(self_pids),
             "agent_pids": sorted(self_agent_pids)},
    "total": runner_n + ext_n,
    "runner_spawned": runner_n,
    "external": ext_n,
    "restart_kills": runner_n,
    "self_agents": self_n,
    "builds": builds_n,
    "runners": runners_n,
    "processes": rows,
    "unreadable_nodes": {"count": len(unreadable), "pids": [u["pid"] for u in unreadable],
                         "processes": unreadable},
    "service_nodes_ignored": {"count": len(service_nodes), "pids": [u["pid"] for u in service_nodes],
                              "processes": service_nodes},
    "ide_checks_ignored": {"count": len(ide_checks), "pids": [r["pid"] for r in ide_checks],
                           "processes": [{k: r[k] for k in ("pid", "ppid", "name", "age_s", "launched_by", "ancestry")}
                                         for r in ide_checks]},
    "service_table": ({"status": "ok", "count": svc_count, "error": None} if svc_by_pid is not None else
                      {"status": "unreadable", "count": None, "error": svc_error} if os_name == "windows" else
                      {"status": "not_applicable", "count": None, "error": None}),
}
unreadable_runner = any(u["runner_descended"] for u in unreadable)
rc = 1 if runner_n else (3 if unreadable_runner else 0)

if mode == "json":
    print(json.dumps(doc))
elif mode == "text":
    print("SESSION CENSUS  %s  host=%s  os=%s" % (doc["as_of"], doc["host"], os_name))
    print("  source: %s (NOT the runner -- see this script's header)" % source)
    print("  self: %s" % (", ".join("%d" % x for x in sorted(self_agent_pids)) or
                          ("resolved, no agent on the chain" if self_pids else "UNRESOLVED -- no start pid was found in the table")))
    print()
    for title, org in (("RUNNER-HOSTED (a runner restart KILLS these)", "runner"),
                       ("EXTERNAL (terminal / IDE / tmux -- a runner restart does NOT touch these)", "external")):
        sel = [r for r in agents if r["origin"] == org]
        print("  %s: %s" % (title, len(sel) if sel else "none"))
        for r in sel:
            print("    pid=%-8s %-7s up=%-8s via %s" % (
                r["pid"], r["kind"], ("%dm" % (r["age_s"] // 60)) if r["age_s"] is not None else "?",
                " <- ".join(r["ancestry"][:3]) or "?"))
    bl = [r for r in rows if r["kind"] == "build" and r["origin"] != "self" and not r["ide_check"]]
    print("\n  BUILDS (cargo/rustc): %s" % (len(bl) if bl else "none"))
    for r in bl:
        print("    pid=%-8s %s  launched_by=%s" % (r["pid"], r["name"], r["launched_by"] or "?"))
    print("  IGNORED rust-analyzer check (an IDE's own cargo/rustc, not a build): %s" % (
        len(ide_checks) if ide_checks else "none"))
    for r in ide_checks:
        print("    pid=%-8s %s  via %s" % (r["pid"], r["name"], " <- ".join(r["ancestry"][:3]) or "?"))
    print("\n  UNREADABLE node (command line could not be read -- cannot rule out an agent): %s" % (
        len(unreadable) if unreadable else "none"))
    for u in unreadable:
        print("    pid=%-8s %s%s via %s%s" % (u["pid"], u["name"], "  RUNNER-DESCENDED" if u["runner_descended"] else "",
                                           " <- ".join(u["ancestry"][:3]) or "?",
                                           ("  (session 0, not a service: %s)" % u["not_a_service"]) if u.get("not_a_service") else ""))
    if service_nodes:
        print("\n  IGNORED unreadable node in Windows session 0 with a proven service shape: %d" % len(service_nodes))
        for u in service_nodes:
            print("    pid=%-8s %s service %s (shape %s) via %s" % (u["pid"], u["name"], u["service_name"], u["service_shape"],
                                                           " <- ".join(u["service_chain"]) or "?"))
    if os_name == "windows" and svc_by_pid is None:
        print("\n  Win32_Service table UNREADABLE (%s): no session-0 node can be proven a service" % svc_error)
    if runner_n:
        verdict = "a runner restart would kill %d live session(s). Check them first." % runner_n
    elif unreadable_runner:
        verdict = "UNKNOWN -- no confirmed runner-hosted session, but a runner-descended node could not be read."
    else:
        verdict = "no runner-hosted sessions -- a runner restart costs no live work."
    print("\n  VERDICT: " + verdict)
sys.exit(rc)
PYEOF

"$PY" "$(native "$WORK/census.py")" "$OS" "$(native "$RAW")" "$SELF_STARTS" \
  "${SESSION_CENSUS_PROC:-/proc}" "$MODE" "$SOURCE"
exit $?
