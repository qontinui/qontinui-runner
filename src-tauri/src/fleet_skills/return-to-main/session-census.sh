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
# ACTIVITY -- OPEN IS NOT WORKING. Every agent row carries `activity`
# (working | idle | stale | unknown), `session_id` and `activity_evidence`
# {status, status_age_s, last_message_age_s, subagent_message_age_s, cpu_delta, children, reason,
# session_id_source}. Plan 2026-09-29-quiet-is-measured-by-session-existence-
# and-machine-wide-so-a-24x7-box-never-gets-one, Phase 1: on the operator's
# Linux box that day 245 sessions were open and 8 had exchanged a message in the last hour,
# so a census that can only say "open" blocks every night. Three signals,
# cross-checked because each alone can be stale:
#   1. Claude Code's own record <config-dir>/sessions/<pid>.json -- `status`
#      (busy | shell | idle | waiting), `statusUpdatedAt`, `sessionId`. The
#      directories come from `claude_registry_dirs` (the fleet's one locator,
#      claude-registry-name.sh in this skill's lib/), plus the process's
#      own $CLAUDE_CONFIG_DIR. The record binds to THIS process only when its
#      `procStart` equals /proc/<pid>/stat's starttime (a crashed session
#      leaves its file behind, and pids are reused).
#   2. The timestamp of the last real `user`/`assistant` line in the session's
#      transcript or in one of its subagents' (<sid>/subagents/*.jsonl -- a
#      session waiting on background agents is working) -- NOT the file mtime (`idle_min`, kept as-is), which tool
#      output and hook writes also bump.
#   3. The process tree's CPU over ONE shared window (default 5 s) for every
#      process together, plus its live descendants. A nested agent's subtree
#      is its own, not its parent's.
# busy/shell is a candidate: `working` needs a message within
# --active-minutes (30) OR CPU above --cpu-floor-pct (10) of one core; both
# arms observed negative is `stale`; an arm that could not be observed is
# `unknown`. idle/waiting is `idle` -- UNLESS a subagent transcript carries a
# message within the window or the tree's CPU clears the floor (background
# agents or a background build run while the main loop sits at a turn
# boundary), which is `working`. The main transcript's own last message never
# upgrades an idle record: a turn that has just ended wrote one. No record, an unparseable one, one bound
# to an earlier process, or any other status value is `unknown` -- the record
# is a Claude Code INTERNAL file, so a format change must read UNKNOWN (which
# blocks), never idle. codex/pi read `unknown` (no reader for them). The axis is
# Linux-only: elsewhere every row reads `unknown` and `activity.status` says why.
# Measured on that box 2026-09-29: one run over ~250 claude processes costs
# the 5 s window plus ~0.6 s; CPU over 5 s was 0.01-0.34 s for 224 idle
# sessions, which is what the 10% floor clears.
#
# WHAT A RUNNER RESTART ACTUALLY KILLS: the `runner`-origin sessions. Sessions
# you started yourself survive it. They die only to a blanket `pkill node` /
# `pkill claude`, which is separately forbidden [policy: production-and-cost
# runner-lifecycle].
#
# Usage:  session-census.sh [--json] [--quiet] [--self-pid PID[,PID...]]
#                           [--active-minutes N] [--cpu-sample-seconds N]
#                           [--cpu-floor-pct N]
#   --json       one JSON object on stdout (the contract machine-quiesce-check.sh
#                reads): {as_of, host, os, source, self:{resolved, starts, pids,
#                agent_pids}, total, runner_spawned, external, restart_kills,
#                self_agents, builds, runners, processes:[{pid, ppid, kind,
#                name, origin, age_s, nested_under_agent, launched_by,
#                ide_check (true | false on a build row, null otherwise),
#                build_attribution (a non-IDE build row: {pid, name, cwd,
#                manifest_path, target_dir, target_dir_source, env_read} of
#                its build ROOT -- see build_attribution(); null otherwise),
#                ancestry, account, cwd, idle_min, session_id, activity,
#                activity_evidence}], activity:{status (ok |
#                not_observable_on_this_os), counts:{working, idle, stale,
#                unknown} over non-self agents, active_minutes,
#                cpu_sample_seconds, cpu_floor_pct, cpu_sampled,
#                registry:{status, dirs}}, unreadable_nodes:{count,
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
#   --active-minutes N      a message this recent makes busy/shell `working` (30)
#   --cpu-sample-seconds N  the one shared CPU window (5; 0 = no sample, so a
#                           busy/shell session with no recent message is `unknown`)
#   --cpu-floor-pct N       CPU above N% of one core over the window is working (10)
#
# Fixture inputs (tests; no live system needed):
#   SESSION_CENSUS_OS        linux | macos | windows   (default: uname)
#   SESSION_CENSUS_PS_FILE   canned process table in that OS's source format
#   SESSION_CENSUS_SELF_PID  self-walk start pid(s), in the table's pid space
#   SESSION_CENSUS_PROC      a /proc root for the Linux environ/cwd reads
#   SESSION_CENSUS_NOW       epoch seconds used as "now"
#   SESSION_CENSUS_REGISTRY_DIRS  colon-separated session-record dirs (a fixture
#                            run reads none unless this is set)
#   SESSION_CENSUS_PROC_AFTER     a second /proc root: the CPU window's closing
#                            snapshot, read instead of sleeping
#   PYTHON                   interpreter to try first
#
# Exit:   0 = no runner-hosted agent session other than self
#         1 = runner-hosted agent sessions are live (a restart kills them)
#         2 = the census itself could not run
#         3 = none confirmed, but a runner-descended `node` has an unreadable
#             command line and may be one -- UNKNOWN, never 0
set -u

JSON=0; QUIET=0; SELF_PID_ARG=""
ACTIVE_MINUTES=30; CPU_SAMPLE_S=5; CPU_FLOOR_PCT=10
while [ $# -gt 0 ]; do
  case "$1" in
    --json)  JSON=1 ;;
    --quiet) QUIET=1 ;;
    --self-pid)
      shift; [ $# -gt 0 ] || { echo "session-census: --self-pid needs a pid" >&2; exit 2; }
      SELF_PID_ARG="$1" ;;
    --active-minutes|--cpu-sample-seconds|--cpu-floor-pct)
      _f="$1"; shift
      case "${1:-}" in ''|*[!0-9]*) echo "session-census: $_f needs a non-negative integer" >&2; exit 2 ;; esac
      case "$_f" in
        --active-minutes) ACTIVE_MINUTES="$1" ;;
        --cpu-sample-seconds) CPU_SAMPLE_S="$1" ;;
        --cpu-floor-pct) CPU_FLOOR_PCT="$1" ;;
      esac ;;
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

# THE SESSION-RECORD DIRECTORIES for the activity axis (see ACTIVITY in the
# header). One locator for the whole fleet: `claude_registry_dirs` in
# claude-registry-name.sh in this skill's lib/ (a private copy, byte-identical to
# scripts/lib/ -- return-to-main-skill-test.sh section I pins that). A fixture
# run (SESSION_CENSUS_PS_FILE) reads NO live directory unless
# SESSION_CENSUS_REGISTRY_DIRS names some: a canned pid must never bind to this
# box's real record for the same number. A missing locator is not "no records":
# it is reported, and every claude reads `unknown`.
REGISTRY_DIRS=""; REGISTRY_STATUS="ok"
if [ -n "${SESSION_CENSUS_REGISTRY_DIRS+set}" ]; then
  REGISTRY_DIRS="$(printf '%s' "$SESSION_CENSUS_REGISTRY_DIRS" | tr ':' '\n')"
elif [ -n "${SESSION_CENSUS_PS_FILE:-}" ]; then
  REGISTRY_STATUS="fixture: no registry directory given"
else
  _sc_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  # The same lib/-else-config-repo fallback recovery-ref-census.sh uses.
  if [ -d "$_sc_dir/lib" ]; then _sc_libdir="$_sc_dir/lib"; else _sc_libdir="$_sc_dir/../../../scripts/lib"; fi
  _sc_lib="$_sc_libdir/claude-registry-name.sh"
  # shellcheck disable=SC1090
  if [ -r "$_sc_lib" ] && . "$_sc_lib" && declare -F claude_registry_dirs >/dev/null 2>&1; then
    REGISTRY_DIRS="$(claude_registry_dirs 2>/dev/null)"
  else
    REGISTRY_STATUS="the session-record locator (claude-registry-name.sh) is not in $_sc_libdir"
  fi
fi

cat >"$WORK/census.py" <<'PYEOF'
import json, os, re, shlex, socket, sys, time
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


# The session holds its scratchpad open, and the scratchpad path carries the
# session UUID: `<tmp>/claude-<uid>/<project>/<uuid>/...`. <tmp> is /tmp by
# default and $TMPDIR when a launcher sets one (measured on the operator's
# Linux box 2026-09-29: `~/.qontinui/scratch/.claude-<acct>/claude-1000/...`, which the
# `/tmp/`-anchored spelling this replaced never matched -- so idle_min was
# null for every session there).
SCRATCH_SID_RE = re.compile(
    r"/claude-\d+/[^/]+/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})(?:/|$)")


# `claude --session-id <uuid>` (how the runner spawns a session): a third
# source for the id when the process has no record and no scratchpad fd.
ARGV_SID_RE = re.compile(
    r"(?:^|\s)--session-id[\s=]+([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})(?:\s|$)")


def linux_fd_session_id(pid):
    fd_dir = os.path.join(proc_root, str(pid), "fd")
    try:
        fds = os.listdir(fd_dir)
    except OSError:
        return None
    for fd in fds:
        try:
            tgt = os.readlink(os.path.join(fd_dir, fd))
        except OSError:
            continue
        m = SCRATCH_SID_RE.search(tgt)
        if m:
            return m.group(1)
    return None


def linux_idle_min(sid, acct):
    """Minutes since the transcript file was last WRITTEN (mtime). Kept as-is
    for its readers; the activity axis uses the last MESSAGE instead, because
    tool output and hook writes bump the mtime too."""
    if not sid or not acct or not os.path.isdir(os.path.join(acct, "projects")):
        return None
    for dp, _dn, fn in os.walk(os.path.join(acct, "projects")):
        if sid + ".jsonl" in fn:
            try:
                return int((now - os.stat(os.path.join(dp, sid + ".jsonl")).st_mtime) // 60)
            except OSError:
                return None
    return None


# ---- ACTIVITY: is this session WORKING, or merely open? ---------------------
# (plan 2026-09-29-quiet-is-measured-by-session-existence-and-machine-wide-so-
# a-24x7-box-never-gets-one, Phase 1 / section 2). The table, exactly:
#   record status busy | shell  -> a CANDIDATE; it is `working` iff
#        a real user/assistant message landed within --active-minutes, OR
#        its process tree burned more than --cpu-floor-pct of one core over
#        the --cpu-sample-seconds window;
#     `stale` iff BOTH arms were observed and both are negative;
#     `unknown` iff an arm that could have said working was unobservable.
#   record status idle | waiting -> `idle`
#   no record, an unparseable one, one bound to an earlier process with this
#   pid, or a status value outside those four -> `unknown`
#   codex / pi -> `unknown` (no activity reader for those harnesses)
#   any OS but Linux -> `unknown` (the pid binding and the CPU arm need /proc)
# UNKNOWN is never folded into idle: a Claude Code format change must degrade
# to today's behaviour (block), not to a false QUIET.
ACTIVE_MINUTES = int(os.environ.get("QSC_ACTIVE_MINUTES") or 30)
CPU_SAMPLE_S = int(os.environ.get("QSC_CPU_SAMPLE_S") or 0)
CPU_FLOOR_PCT = int(os.environ.get("QSC_CPU_FLOOR_PCT") or 10)
CPU_FLOOR_S = CPU_SAMPLE_S * CPU_FLOOR_PCT / 100.0
REGISTRY_STATUS = os.environ.get("QSC_REGISTRY_STATUS") or "ok"
REGISTRY_DIRS = [d for d in (os.environ.get("QSC_REGISTRY_DIRS") or "").splitlines() if d.strip()]
CANDIDATE_STATUSES = {"busy", "shell"}
IDLE_STATUSES = {"idle", "waiting"}
# The tail of a transcript read for the last message. A session whose last
# message is further back than this many bytes has not spoken for a long time;
# the arm then reads UNOBSERVED (never "no message"), so it cannot make a
# session `stale` on its own.
TRANSCRIPT_TAIL_MAX = 8 * 1024 * 1024
try:
    CLK_TCK = os.sysconf("SC_CLK_TCK")
except (AttributeError, ValueError, OSError):
    CLK_TCK = 100


def proc_stat_table(root):
    """{pid: (ppid, cumulative cpu seconds incl. reaped children, starttime)}
    over every numeric entry of a /proc root. ONE pass for the whole box."""
    out = {}
    try:
        names = os.listdir(root)
    except OSError:
        return None
    for n in names:
        if not n.isdigit():
            continue
        try:
            with open(os.path.join(root, n, "stat"), "rb") as fh:
                raw = fh.read().decode("utf-8", "replace")
        except OSError:
            continue
        rest = raw.rsplit(")", 1)[-1].split()
        try:
            ppid = int(rest[1])
            cpu = sum(int(x) for x in rest[11:15]) / float(CLK_TCK)  # utime stime cutime cstime
            start = rest[19]
        except (IndexError, ValueError):
            continue
        out[int(n)] = (ppid, cpu, start)
    return out


def read_record(path):
    try:
        with open(path, "rb") as fh:
            rec = json.loads(fh.read().decode("utf-8", "replace"))
    except (OSError, ValueError):
        return None
    return rec if isinstance(rec, dict) else None


def find_record(pid, acct, starttime, created):
    """(record, path, None) for the record bound to THIS process, else
    (None, path-or-None, reason)."""
    dirs = []
    if acct:
        dirs.append(os.path.join(acct.rstrip("/"), "sessions"))
    dirs += REGISTRY_DIRS
    seen, paths = set(), []
    for d in dirs:
        d = os.path.normpath(d)
        if d in seen:
            continue
        seen.add(d)
        f = os.path.join(d, "%d.json" % pid)
        if os.path.isfile(f):
            paths.append(f)
    if not paths:
        if REGISTRY_STATUS != "ok" and not acct:
            return None, None, "no session record: %s" % REGISTRY_STATUS
        return None, None, "no session record <config-dir>/sessions/%d.json in %d searched director%s" % (
            pid, len(seen), "y" if len(seen) == 1 else "ies")
    bound, why = [], []
    for f in paths:
        rec = read_record(f)
        if rec is None:
            why.append("%s is unparseable" % f)
            continue
        if rec.get("pid") != pid:
            why.append("%s names pid %r" % (f, rec.get("pid")))
            continue
        ps = rec.get("procStart")
        if ps is not None and starttime is not None and str(ps) != str(starttime):
            why.append("%s belongs to an earlier process with this pid (procStart %s, live %s)" % (f, ps, starttime))
            continue
        sa = rec.get("startedAt")
        if (ps is None and isinstance(sa, (int, float)) and not isinstance(sa, bool)
                and created is not None and sa / 1000.0 < created - 5):
            why.append("%s predates this process (startedAt before its start)" % f)
            continue
        bound.append((rec, f))
    if not bound:
        return None, paths[0], "; ".join(why)
    sids = {str(r.get("sessionId")) for r, _ in bound}
    if len(sids) > 1:
        return None, bound[0][1], "%d records bind this pid to different sessions" % len(bound)
    return bound[0][0], bound[0][1], None


_project_index = {}


def transcript_path(cfg_dir, cwd, sid):
    projects = os.path.join(cfg_dir, "projects")
    if isinstance(cwd, str) and cwd:
        cand = os.path.join(projects, re.sub(r"[^A-Za-z0-9]", "-", cwd), sid + ".jsonl")
        if os.path.isfile(cand):
            return cand
    # Claude Code shortens a long project-dir name, so fall back to an index of
    # this config dir's project folders -- built once per config dir.
    idx = _project_index.get(projects)
    if idx is None:
        idx = {}
        try:
            for pd in os.listdir(projects):
                try:
                    for fn in os.listdir(os.path.join(projects, pd)):
                        if fn.endswith(".jsonl"):
                            idx.setdefault(fn[:-6], os.path.join(projects, pd, fn))
                except OSError:
                    continue
        except OSError:
            pass
        _project_index[projects] = idx
    return idx.get(sid)


def iso_epoch(v):
    if not isinstance(v, str):
        return None
    m = re.match(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(?:\.(\d+))?Z$", v)
    if not m:
        return None
    base = datetime.strptime(m.group(1), "%Y-%m-%dT%H:%M:%S").replace(tzinfo=timezone.utc).timestamp()
    return base + (float("0." + m.group(2)) if m.group(2) else 0.0)


def last_message_epoch(path):
    """(epoch of the last real user/assistant line, complete). `complete` is
    True when the whole file was scanned, so a None epoch then means "none
    ever" rather than "not within the tail read"."""
    try:
        size = os.path.getsize(path)
        fh = open(path, "rb")
    except OSError:
        return None, False
    with fh:
        block, buf, pos = 65536, b"", size
        while pos > 0 and size - pos < TRANSCRIPT_TAIL_MAX:
            step = min(block, pos)
            pos -= step
            fh.seek(pos)
            buf = fh.read(step) + buf
            lines = buf.split(b"\n")
            # lines[0] may be a partial line unless the read reached the start.
            head, body = (lines[0], lines[1:]) if pos > 0 else (b"", lines)
            for ln in reversed(body):
                if b'"type":"user"' not in ln and b'"type":"assistant"' not in ln:
                    continue
                try:
                    d = json.loads(ln.decode("utf-8", "replace"))
                except ValueError:
                    continue
                if not isinstance(d, dict) or d.get("type") not in ("user", "assistant") or d.get("isMeta") is True:
                    continue
                ts = iso_epoch(d.get("timestamp"))
                if ts is not None:
                    return ts, True
            buf = head
            block = min(block * 2, 1024 * 1024)
        return None, pos == 0


def tree_of(pid, table, stop):
    """pid plus every descendant in `table`, not descending into a pid in
    `stop` (a nested agent owns its own subtree)."""
    kids = {}
    for c, (pp, _cpu, _st) in table.items():
        kids.setdefault(pp, []).append(c)
    out, stack = [], [pid]
    while stack:
        x = stack.pop()
        out.append(x)
        for c in kids.get(x, ()):
            if c not in stop and c not in out:
                stack.append(c)
    return out


def classify_activity(rec, rec_why, msg_age, msg_observed, cpu_delta, sub_age=None):
    if rec is None:
        return "unknown", rec_why
    st = rec.get("status")
    if st in IDLE_STATUSES:
        # `idle` / `waiting` describe the MAIN loop only: a session at a turn
        # boundary whose background subagents are still talking, or whose tree
        # is burning CPU (a run_in_background build), is working. The main
        # transcript's own last message is deliberately NOT an upgrade -- a turn
        # that just ended wrote one.
        if sub_age is not None and sub_age <= ACTIVE_MINUTES * 60:
            return "working", "record status %s, but a subagent message %ds ago (within %d min)" % (st, sub_age, ACTIVE_MINUTES)
        if cpu_delta is not None and cpu_delta > CPU_FLOOR_S:
            return "working", "record status %s, but %.2f cpu-s over %ds (floor %.2f)" % (st, cpu_delta, CPU_SAMPLE_S, CPU_FLOOR_S)
        return "idle", "record status %s" % st
    if st not in CANDIDATE_STATUSES:
        return "unknown", "record status %r is not one of busy/shell/idle/waiting" % (st,)
    if msg_age is not None and msg_age <= ACTIVE_MINUTES * 60:
        return "working", "record status %s and a message %ds ago (within %d min)" % (st, msg_age, ACTIVE_MINUTES)
    if cpu_delta is not None and cpu_delta > CPU_FLOOR_S:
        return "working", "record status %s and %.2f cpu-s over %ds (floor %.2f)" % (st, cpu_delta, CPU_SAMPLE_S, CPU_FLOOR_S)
    if msg_observed and cpu_delta is not None:
        return "stale", "record status %s but no message within %d min and %.2f cpu-s over %ds (floor %.2f)" % (
            st, ACTIVE_MINUTES, cpu_delta, CPU_SAMPLE_S, CPU_FLOOR_S)
    return "unknown", "record status %s, no message within %d min, and the %s could not be observed" % (
        st, ACTIVE_MINUTES, "CPU sample" if cpu_delta is None else "last message")


# A build's ATTRIBUTION -- which checkout it reads and writes -- comes from its
# build ROOT: the outermost process on its chain that is still part of the
# build (the `cargo` above a `rustc` or a build script; a rustc's own cwd is
# often a registry crate's). Read there: the cwd (Linux /proc only), a
# `--manifest-path` / `--target-dir` on its command line, and CARGO_TARGET_DIR
# / CARGO_BUILD_TARGET_DIR in its environment (Linux only). A field that could
# not be read is null -- the consumer decides what an unattributable build
# blocks; this census never guesses.
def _flag_value(tokens, flag):
    for i, t in enumerate(tokens):
        if t == flag and i + 1 < len(tokens):
            return tokens[i + 1]
        if t.startswith(flag + "="):
            return t[len(flag) + 1:]
    return None


def build_attribution(p, anc):
    top = p
    for a in anc:
        if a["kind"] == "build" or a["norm"] in BUILD_HOPS or a["norm"].startswith("build-script-"):
            top = a
            continue
        break
    try:
        toks = shlex.split(top["cmd"] or "", posix=(os_name != "windows"))
    except ValueError:
        toks = (top["cmd"] or "").split()
    toks = [t.strip('"') for t in toks]
    target, source = _flag_value(toks, "--target-dir"), None
    if target:
        source = "flag"
    env = linux_env(top["pid"]) if os_name == "linux" else {}
    if not target:
        for k in ("CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"):
            if env.get(k):
                target, source = env[k], "env:" + k
                break
    return {"pid": top["pid"], "name": top["name"],
            "cwd": linux_cwd(top["pid"]) if os_name == "linux" else None,
            "manifest_path": _flag_value(toks, "--manifest-path"),
            "target_dir": target, "target_dir_source": source,
            # Whether the ENVIRONMENT was readable at all: a null target_dir
            # with env_read true means cargo's default (<workspace>/target).
            "env_read": bool(env) if os_name == "linux" else False}


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
    fd_sid = linux_fd_session_id(p["pid"]) if os_name == "linux" and kind == "claude" else None
    m = ARGV_SID_RE.search(p["cmd"] or "") if kind == "claude" else None
    argv_sid = m.group(1) if m else None
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
        "build_attribution": build_attribution(p, anc) if kind == "build" and not ide_check else None,
        "ancestry": ["%s(%d)" % (a["name"], a["pid"]) for a in anc[:8]],
        "account": os.path.basename(acct.rstrip("/")) if acct else None,
        "cwd": linux_cwd(p["pid"]) if os_name == "linux" and kind in AGENTS else None,
        "idle_min": linux_idle_min(fd_sid, acct) if os_name == "linux" and kind == "claude" else None,
        "session_id": None,
        "activity": None,
        "activity_evidence": None,
        "_acct": acct,
        "_fd_sid": fd_sid,
        "_argv_sid": argv_sid,
        "_created": p["created"],
    })


# ---- the activity pass (see ACTIVITY above) --------------------------------
agent_pids = {r["pid"] for r in rows if r["kind"] in AGENTS}
activity_status = "ok" if os_name == "linux" else "not_observable_on_this_os"
t0_stat = proc_stat_table(proc_root) if os_name == "linux" else None
t0_clock = time.time()
pending = []
for r in rows:
    if r["kind"] not in AGENTS:
        continue
    ev = {"status": None, "status_age_s": None, "last_message_age_s": None,
          "cpu_delta": None, "children": None, "reason": None, "session_id_source": None}
    r["activity_evidence"] = ev
    if r["kind"] != "claude":
        r["activity"] = "unknown"
        ev["reason"] = "no activity reader for a %s session" % r["kind"]
        continue
    if os_name != "linux":
        r["activity"] = "unknown"
        ev["reason"] = ("the activity axis is Linux-only: the record's pid binding and the CPU arm read /proc, "
                        "which %s does not have" % os_name)
        continue
    start = t0_stat.get(r["pid"], (None, None, None))[2] if t0_stat else None
    rec, _path, why = find_record(r["pid"], r["_acct"], start, r["_created"])
    if rec is not None:
        ev["status"] = rec.get("status") if isinstance(rec.get("status"), str) else repr(rec.get("status"))
        su = rec.get("statusUpdatedAt")
        if isinstance(su, (int, float)) and not isinstance(su, bool):
            ev["status_age_s"] = int(now - su / 1000.0)
        rsid = rec.get("sessionId") if isinstance(rec.get("sessionId"), str) else None
        if rsid and r["_fd_sid"] and rsid != r["_fd_sid"]:
            # The record is rewritten by the live process; the scratchpad fd can
            # lag a /clear or an in-process /resume. The record wins, and the
            # disagreement is shown rather than hidden.
            ev["session_id_fd"] = r["_fd_sid"]
        msg_age, msg_observed, sub_age = None, False, None
        if rsid:
            tp = transcript_path(os.path.dirname(os.path.dirname(_path)), rec.get("cwd"), rsid)
            if tp:
                ts, complete = last_message_epoch(tp)
                # A session waiting on its own background subagents talks in
                # <sid>/subagents/*.jsonl, not in its main transcript. Only a
                # file WRITTEN inside the window can hold a message inside it,
                # so the mtime filter keeps this to the live few.
                sub = os.path.join(tp[:-6], "subagents")
                try:
                    subs = [os.path.join(sub, f) for f in os.listdir(sub) if f.endswith(".jsonl")]
                except OSError:
                    subs = []
                for sf in subs:
                    try:
                        if os.stat(sf).st_mtime < now - ACTIVE_MINUTES * 60:
                            continue
                    except OSError:
                        continue
                    sts, _c = last_message_epoch(sf)
                    if sts is not None and (ts is None or sts > ts):
                        ts = sts
                    if sts is not None and (sub_age is None or now - sts < sub_age):
                        sub_age = max(0, int(now - sts))
                if ts is not None:
                    msg_age, msg_observed = max(0, int(now - ts)), True
                elif complete:
                    msg_observed = True  # the whole transcript holds no message yet
        ev["last_message_age_s"] = msg_age
        ev["subagent_message_age_s"] = sub_age
        r["_rec"], r["_why"], r["_msg"] = rec, None, (msg_age, msg_observed, sub_age)
    else:
        rsid = None
        r["_rec"], r["_why"], r["_msg"] = None, why, (None, False, None)
    for src, v in (("record", rsid), ("scratchpad_fd", r["_fd_sid"]), ("argv", r["_argv_sid"])):
        if v:
            r["session_id"], ev["session_id_source"] = v, src
            break
    pending.append(r)

# ONE CPU window for every process together -- never a window per process
# (~250 of them on a busy box). It is opened by t0_stat above, overlaps the
# record and transcript reads, and is only waited out when some candidate
# still needs it; SESSION_CENSUS_PROC_AFTER supplies the second snapshot in a
# fixture (no sleep).
# Every bound record needs it: busy/shell to tell working from stale, and
# idle/waiting for the cross-check (a background build under an idle session).
need_cpu = any(r["_rec"] is not None for r in pending)
t1_stat = None
if pending and t0_stat is not None and CPU_SAMPLE_S > 0 and need_cpu:
    after = os.environ.get("SESSION_CENSUS_PROC_AFTER")
    if after:
        t1_stat = proc_stat_table(after)
    else:
        rest = CPU_SAMPLE_S - (time.time() - t0_clock)
        if rest > 0:
            time.sleep(rest)
        t1_stat = proc_stat_table(proc_root)
for r in pending:
    ev = r["activity_evidence"]
    cpu = None
    if t1_stat is not None and r["pid"] in t1_stat and r["pid"] in t0_stat:
        tree = tree_of(r["pid"], t1_stat, agent_pids - {r["pid"]})
        cpu = 0.0
        for x in tree:
            b = t1_stat.get(x)
            a = t0_stat.get(x)
            cpu += b[1] - (a[1] if a is not None and a[2] == b[2] else 0.0)
        cpu = round(max(cpu, 0.0), 3)
        ev["children"] = len(tree) - 1
    elif t0_stat is not None and r["pid"] in t0_stat:
        ev["children"] = len(tree_of(r["pid"], t0_stat, agent_pids - {r["pid"]})) - 1
    ev["cpu_delta"] = cpu
    msg_age, msg_observed, sub_age = r["_msg"]
    r["activity"], ev["reason"] = classify_activity(r["_rec"], r["_why"], msg_age, msg_observed, cpu, sub_age)
for r in rows:
    for k in [k for k in r if k.startswith("_")]:
        del r[k]

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
act_counts = {k: 0 for k in ("working", "idle", "stale", "unknown")}
for r in agents:
    if r["origin"] != "self":
        act_counts[r["activity"]] += 1

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
    "activity": {"status": activity_status, "counts": act_counts,
                 "active_minutes": ACTIVE_MINUTES, "cpu_sample_seconds": CPU_SAMPLE_S,
                 "cpu_floor_pct": CPU_FLOOR_PCT, "cpu_sampled": t1_stat is not None,
                 "registry": {"status": REGISTRY_STATUS, "dirs": len(REGISTRY_DIRS)}},
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
    print("\n  ACTIVITY (non-self agents; a session record + last message + one %ds CPU window): "
          "working %d, idle %d, stale %d, unknown %d%s" % (
              CPU_SAMPLE_S, act_counts["working"], act_counts["idle"], act_counts["stale"], act_counts["unknown"],
              "" if activity_status == "ok" else "  (%s)" % activity_status))
    for r in agents:
        if r["origin"] != "self" and r["activity"] in ("working", "stale"):
            print("    pid=%-8s %-7s %-7s %s" % (r["pid"], r["activity"], r["origin"], r["activity_evidence"]["reason"]))
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

QSC_REGISTRY_DIRS="$REGISTRY_DIRS" QSC_REGISTRY_STATUS="$REGISTRY_STATUS" \
QSC_ACTIVE_MINUTES="$ACTIVE_MINUTES" QSC_CPU_SAMPLE_S="$CPU_SAMPLE_S" QSC_CPU_FLOOR_PCT="$CPU_FLOOR_PCT" \
"$PY" "$(native "$WORK/census.py")" "$OS" "$(native "$RAW")" "$SELF_STARTS" \
  "${SESSION_CENSUS_PROC:-/proc}" "$MODE" "$SOURCE"
exit $?
