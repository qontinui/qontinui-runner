#!/usr/bin/env bash
# machine-quiesce-check.sh - is THIS machine quiet enough for an unattended job
# to move checkouts? QUIET / BUSY / UNKNOWN, with every signal it read.
#
# Plan 2026-09-13-nightly-return-to-main-sweep, Phase 2b; per-resource verdicts
# (--for) by plan 2026-09-29-quiet-is-measured-by-session-existence-and-machine-
# wide-so-a-24x7-box-never-gets-one, Phase 3. The consumer is the
# `/return-to-main` skill: it asks `--for checkout-ff` and `--for
# checkout-return` and acts on each repo whose own verdict reads QUIET, even
# while the machine-wide verdict reads BUSY. Without --for the output is
# exactly what it was before --for existed (pinned byte for byte by
# machine-quiesce-check-test.sh section G).
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
#                        default 120 s); none present -> not_applicable; a
#                        STALE_AFTER cargo-guard.sh refuses (lock-config-
#                        invalid: not digits, a leading zero, over 9
#                        digits, or not above the heartbeat) -> unknown
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
# PER-RESOURCE VERDICTS (--for). A machine-wide verdict answers "may anything
# move?", and on a box with ~250 open sessions it is BUSY every hour of every
# day. The question a checkout move needs is narrower: "may THIS move happen to
# THIS repo?" -- and it depends on what the move does (the plan's section 2):
#   checkout-ff      (fast-forward R's default branch; restore proven residue
#                    in R) is blocked by: a session WORKING -- census activity
#                    `working`, or `unknown`, which is never quiet -- whose
#                    touch-ledger touches within --touch-window (6h) name R
#                    (primary or a linked worktree of R, which shares R's
#                    .git), or include a SPECIFIC unattributed one (it names
#                    somewhere the ledger cannot: an unresolvable cd / -C /
#                    --manifest-path / --prefix / --directory / --cwd target,
#                    an Edit/Write path it cannot resolve, an unreadable
#                    envelope, an unresolved root -- every repo), or whose
#                    attribution is UNKNOWN
#                    (no session id, no ledger for it, no ledger directory --
#                    every repo). A working session whose unattributed touches
#                    in the window are all GENERIC (the no-path marker, a
#                    "generic":true `$` argument) blocks, beyond its touched
#                    and cwd repos, only each R whose own ref records moved
#                    inside the window -- a GENERIC line can hide a ref move
#                    (GIT_DIR=, --git-dir=, -c core.worktree=, xargs {}, a
#                    script). Those records are R's logs/HEAD, the log of the
#                    branch HEAD names and logs/refs/stash, each by its newest
#                    entry's time or the file's mtime, whichever is later (an
#                    exported GIT_COMMITTER_DATE backdates an entry; a
#                    commit's own date is never read), and the mtimes of HEAD,
#                    of the loose refs/stash (`git update-ref refs/stash`
#                    logs nothing when refs/stash has no log yet, unless
#                    --create-reflog or core.logAllRefUpdates=always) and
#                    of that branch's loose ref -- or, when it has none, of
#                    packed-refs and of the directory a loose ref was
#                    unlinked from. It also blocks each R whose working tree
#                    has a change written inside the window: a TRACKED
#                    change, or an UNTRACKED file (an ignored file never
#                    counts). git status --untracked-files=all
#                    (--no-optional-locks, so it never takes index.lock in a
#                    shared checkout or rewrites the index;
#                    core.fsmonitor=false, so no file-system monitor answers
#                    for it or is started by it) lists them, dated by the
#                    index's, each dirty path's own (an untracked file's
#                    exactly as a tracked one's) and, for a deleted path, its
#                    nearest existing parent's mtime OR ctime, whichever is
#                    later (cp -p, rsync -a, tar x, touch -d, mv of an older
#                    file and chmod keep or backdate an mtime, not a ctime);
#                    an old dirty tree stays QUIET, untracked files included.
#                    That dating is POSIX's: on Windows st_ctime is the
#                    CREATION time, which none of those writes moves, so
#                    there ANY change, an untracked file included, blocks R
#                    however old (the safe direction: no stale residue is
#                    restored or removed there while such a session works).
#                    RESIDUAL, stated: the .git internals above are read by
#                    mtime and entries only (a fixture cannot age a ctime), so
#                    one rewritten with a preserved or backdated mtime and no
#                    log entry is not seen; a stash deleted outright (git
#                    stash clear, or a drop of its last entry, removes the ref
#                    and its log alike) leaves nothing to read, and the sweep
#                    never touches a stash; and git status, to compare a
#                    stat-dirty tracked file's content, runs any clean filter
#                    configured for it (git-lfs's among them) -- a process
#                    this read starts, as repo_work()'s status does too, and
#                    one that may write its own store (git-lfs writes
#                    .git/lfs/objects). R
#                    also blocks when any of that cannot be read (HEAD naming
#                    neither a branch nor an object id; a missing, unreadable,
#                    torn, unparseable or emptied HEAD or branch log; an
#                    unreadable stash log or refs/stash; a git dir rev-parse
#                    cannot resolve; a failing git status; any exception
#                    reading R, which is R's UNKNOWN and is named on stderr,
#                    never the whole probe's crash: UNKNOWN, never "no move").
#                    RISKS, stated: checkout-ff QUIET also gates
#                    --restore-residue, which reverts tracked files to a
#                    classed upstream state AND unlinks each untracked file
#                    byte-identical to the upstream tip, even in a tree with
#                    no tracked change -- which is why the tree's recency
#                    counts, untracked files included, not ref moves alone
#                    (`cp $W/x $R/x` of the tip's x, or `git
#                    --git-dir=$R/.git show origin/main:x > $R/x`, writes one
#                    a restore would unlink). Every untracked file counts,
#                    not only one at a path the last-fetched tip has: the
#                    sweep fetches before it restores, so a stale
#                    origin/<default> cannot bound which ones it unlinks --
#                    and so an untracked file something keeps writing (a log
#                    nothing ignores) keeps R BUSY while such a session
#                    works: the safe direction. A long-running READER of R
#                    that the ledger saw only through a `$` path
#                    (`grep -r x $R`, a script reading `$R/...`, its cwd
#                    elsewhere) no longer makes R BUSY: a read writes no ref
#                    record and no tracked change, so a fast-forward can
#                    change files under a read in progress (a build the census
#                    sees still blocks R, by the build rule below). The
#                    sweep's own fast-forward, and any pull, writes R's
#                    logs/HEAD, so R stays BUSY for a window after it while a
#                    GENERIC-only session works: the safe direction. Each
#                    primary costs one read of HEAD and one git rev-parse,
#                    plus a git status only when those leave it undecided (not
#                    moved, not unreadable) -- each retried once, so one to
#                    four git calls, each bounded at 30 s; a failure or a
#                    timeout is that primary's UNKNOWN, so it blocks. Nothing
#                    bounds the whole run to the scheduler's 120 s probe
#                    ceiling (schedule-return-to-main.sh
#                    PROBE_TIMEOUT_SECONDS): a probe that runs past it is
#                    killed and reads NOT met (the runner's
#                    scheduler_probe.rs), so the job does not fire -- the
#                    safe direction.
#                    A ledger reader that serves no specific_* keeps the old
#                    rule: any unattributed touch in the window blocks every
#                    repo. Also: a session whose cwd is in R; a build that
#                    READS R's working tree (its source is R, or a path
#                    dependency lands in R; a build sourced in a linked
#                    worktree reads no primary; a build whose source cannot be
#                    read falls back to its target dir's owner, and with no
#                    target either blocks every repo); a fresh cargo-guard lock
#                    in R whose holder's build is not in the census; a
#                    supervisor build slot (the runner and its path deps); a
#                    runner AI session whose cwd or worktree names R (else
#                    every repo); a runner-listed session the census does not
#                    see; codex/pi (no activity or attribution reader: every
#                    repo); a git index.lock / HEAD.lock in R. NOT by idle,
#                    waiting or stale sessions anywhere, nor by builds of other
#                    repos, nor by coord rows (not read under checkout-ff: a
#                    fast-forward kills no session).
#   checkout-return  (switch R off a parked branch) is blocked by all of the
#                    above -- except that the GENERIC confinement is
#                    checkout-ff's alone: here ANY unattributed touch by a
#                    working session within the window still blocks every
#                    repo, unchanged (a return switches branches, and the
#                    lifetime rule below is blind to a switch that landed
#                    GENERIC) -- PLUS every session not `finished` (coord's
#                    sessionStatus; an unreadable coord reads "not finished")
#                    that EVER touched R's primary checkout -- its next commit
#                    would land on the wrong branch. Whole-life attribution is
#                    UNKNOWN (so: every repo) for a session with no ledger, a
#                    truncated one, or a SPECIFIC unattributed touch (an
#                    unresolvable cd / git -C target). A generic unattributed
#                    touch (a command naming no path) does not widen it, and a
#                    worktree-only touch never blocks the primary's return.
#   process-restart  today's machine-wide rule, unchanged: every repo takes
#                    the machine verdict and its blocking list.
# per_repo[R].verdict is BUSY when anything blocks R, else UNKNOWN when a
# machine-level probe the move stands on read unknown (census, runner
# instances, readiness, runner coverage, supervisor builds), else QUIET.
# UNKNOWN activity and UNKNOWN attribution are the blocking case, never quiet
# [policy: verification-and-evidence silent-empty-is-unknown].
#
# THE TOUCH LEDGER is read once, with touch-ledger-read.sh --by-session
# --sessions <every census session id>: $QMQC_TOUCH_LEDGER_READ, else beside
# this script, else the config repo's scripts/ under the workspace root, else
# the config repo this skill directory sits in. Not found, a missing ledger directory, or
# an unparseable answer is the `touch_ledger` probe reading unknown -- which
# makes every WORKING session block every repo rather than any repo UNKNOWN,
# because it is attribution that is missing, not the machine's state.
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
#                                 [--for checkout-ff|checkout-return|process-restart
#                                  [--touch-window DUR] [--exit-quiet-if-any-repo]]
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
#   --for CLASS         add per-resource verdicts (above). The JSON gains
#                       `for`, `per_repo_summary{counts, act_eligible,
#                       touch_window_s}` and, per repo, `verdict`
#                       (QUIET|BUSY|UNKNOWN), `blocking[]` (at most 20 entries,
#                       each with `class`, `why` and its identifying fields),
#                       `blocking_count`, `blocking_truncated` when capped, and
#                       `unknown_because` on an UNKNOWN; `per_repo[R]` drops the
#                       newest_transcript_* guess it replaces, and a
#                       `touch_ledger` probe is added. The top-level verdict is
#                       computed as without --for over what was read (under
#                       checkout-ff that excludes coord rows, which are not read).
#   --touch-window DUR  how recent a working session's touch must be to count
#                       (<n>s|m|h|d, default 6h)
#   --exit-quiet-if-any-repo
#                       (needs --for checkout-ff or checkout-return) the EXIT
#                       CODE answers "is there work the sweep may do now?" --
#                       the runner scheduler's probe condition. For each QUIET
#                       repo, cheaply and with no fetch, "something to do" is:
#                       checkout-ff -- on its origin default branch and behind
#                       origin/<default> as last fetched (`git rev-list --count
#                       HEAD..origin/<default>`), or modified TRACKED files
#                       (`git --no-optional-locks status --porcelain
#                       --untracked-files=no`). An untracked file alone is
#                       not, although a residue restore does unlink one that
#                       equals the upstream tip's file at its path: level with
#                       origin/<default> that path is tracked, and behind it
#                       the behind-count is work already; a parked or ahead
#                       checkout whose only dirt is such a file waits for a
#                       run another repo fires (repo_work() says why);
#                       checkout-return -- HEAD not on the default branch. A
#                       git read that fails counts as work.
#                       The first QUIET repo with work stops the scan, and each
#                       repo read carries per_repo[R].work{has_work, reason}.
#
# Fixture inputs (tests; no live system needed):
#   QMQC_FIXTURE_DIR    canned HTTP: $DIR/http/<name>.status holds `200` (or any
#                       code), `refused` or `timeout`; <name>.body the body.
#                       Names: sup_runners, sup_builds, readiness_<port>,
#                       coord_proxy_fleet_<page>, coord_credential,
#                       coord_fleet_<page>
#   QMQC_CENSUS_BIN     the census to run (default: beside this script)
#   QMQC_TOUCH_LEDGER_READ  the touch-ledger reader (default: resolved as above)
#   QONTINUI_TOUCH_LEDGER_DIR  passed through to the reader
#   QMQC_LIB_DIR        where hook-json.sh is (default: lib/ beside this script)
#   QMQC_NOW            epoch seconds used as "now"
#   QMQC_COORD_URL      coord base (default $COORD_HTTP_URL, then
#                       https://coord.qontinui.io)
#   SESSION_CENSUS_*    passed through to the census (see its header)
#
# Exit: 0 QUIET, 1 BUSY, 3 UNKNOWN, 4 usage -- the MACHINE-WIDE verdict, with
# or without --for. Under --exit-quiet-if-any-repo instead:
#   0 some QUIET repo has something to do (act now)
#   5 at least one repo is QUIET, and none of them has anything to do
#   1 no repo is QUIET and at least one is BUSY
#   3 no repo is QUIET and none is BUSY (every repo UNKNOWN)
#   4 usage
set -u

ROOT_ARG=""; JSON=0
SELF_SID="${CLAUDE_CODE_SESSION_ID:-}"
READINESS_TIMEOUT=60; HTTP_TIMEOUT=20
FOR=""; EXIT_QUIET=0; TOUCH_WINDOW=6h
usage() { sed -n '2,/^set -u$/{/^set -u$/d;p;}' "$0"; }
while [ $# -gt 0 ]; do
  case "$1" in
    --root) shift; [ $# -gt 0 ] || { echo "machine-quiesce-check: --root needs a directory" >&2; exit 4; }; ROOT_ARG="$1" ;;
    --json) JSON=1 ;;
    --self-session-id) shift; [ $# -gt 0 ] || { echo "machine-quiesce-check: --self-session-id needs a value" >&2; exit 4; }; SELF_SID="$1" ;;
    --readiness-timeout) shift; case "${1:-}" in ''|*[!0-9]*) echo "machine-quiesce-check: --readiness-timeout needs seconds" >&2; exit 4 ;; esac; READINESS_TIMEOUT="$1" ;;
    --http-timeout) shift; case "${1:-}" in ''|*[!0-9]*) echo "machine-quiesce-check: --http-timeout needs seconds" >&2; exit 4 ;; esac; HTTP_TIMEOUT="$1" ;;
    --for) shift; case "${1:-}" in checkout-ff|checkout-return|process-restart) FOR="$1" ;; *) echo "machine-quiesce-check: --for takes checkout-ff, checkout-return or process-restart" >&2; exit 4 ;; esac ;;
    --exit-quiet-if-any-repo) EXIT_QUIET=1 ;;
    --touch-window) shift; case "${1:-}" in *[0-9][smhd]) TOUCH_WINDOW="$1" ;; *) echo "machine-quiesce-check: --touch-window takes <n>s|<n>m|<n>h|<n>d" >&2; exit 4 ;; esac ;;
    -h|--help) usage; exit 0 ;;
    *) echo "machine-quiesce-check: unknown argument: $1" >&2; exit 4 ;;
  esac
  shift
done
case "$SELF_SID" in
  *[!0-9A-Za-z-]*) echo "machine-quiesce-check: --self-session-id must be a session UUID" >&2; exit 4 ;;
esac
if [ "$EXIT_QUIET" = 1 ]; then
  case "$FOR" in
    checkout-ff|checkout-return) ;;
    *) echo "machine-quiesce-check: --exit-quiet-if-any-repo needs --for checkout-ff or --for checkout-return (a process restart has no per-repo work)" >&2; exit 4 ;;
  esac
fi
case "$TOUCH_WINDOW" in
  *s) TOUCH_WINDOW_S="${TOUCH_WINDOW%s}" ;;
  *m) TOUCH_WINDOW_S=$(( ${TOUCH_WINDOW%m} * 60 )) ;;
  *h) TOUCH_WINDOW_S=$(( ${TOUCH_WINDOW%h} * 3600 )) ;;
  *d) TOUCH_WINDOW_S=$(( ${TOUCH_WINDOW%d} * 86400 )) ;;
esac 2>/dev/null
case "${TOUCH_WINDOW_S:-}" in ''|*[!0-9]*) echo "machine-quiesce-check: --touch-window takes <n>s|<n>m|<n>h|<n>d" >&2; exit 4 ;; esac

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
    # The header set this request WOULD have sent, kept beside the log so a
    # suite can assert what a credential resolved to (fixture mode only; the
    # values there are synthetic).
    if [ -n "$hdr" ] && [ -f "$hdr" ]; then mkdir -p "$QMQC_FIXTURE_DIR/hdr" && cp "$hdr" "$QMQC_FIXTURE_DIR/hdr/$name"; fi
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
import glob, json, os, re, subprocess, sys, time
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


def expand_env_ref_for_url(value, url):
    """`${NAME:-default}` -> this process's NAME when set and non-empty, else the
    default - reading the environment ONLY for a strictly-loopback `url` (any
    other URL resolves on the default arm, so no environment value leaves this
    box); a no-default reference raises ValueError (UNEXPANDED_ENV_REF). The one
    python implementation is lib/mcp_env_ref.py (plan
    2026-09-22-one-coord-mcp-nonce-per-terminal-so-the-terminal-leg-engages);
    without it a value that carries a reference is refused, never sent."""
    lib = os.environ.get("QMQC_PY_LIB") or ""
    try:
        if lib and lib not in sys.path:
            sys.path.insert(0, lib)
        from mcp_env_ref import expand_env_ref_for_url as _expand
    except ImportError:
        start = value.find("${")
        if start != -1 and value.find("}", start + 2) != -1:
            raise ValueError("UNEXPANDED_ENV_REF (helper absent): lib/mcp_env_ref.py not found - "
                             "refusing to send a ${...} reference literally (LOCAL fault)")
        return value
    return _expand(value, url)


def cmd_proxy(mcp_json, hdr_out):
    """Write the coord-mcp proxy's headers to hdr_out; print its URL base."""
    doc = load(mcp_json)
    srv = ((doc or {}).get("mcpServers") or {}).get("coord-mcp") if isinstance(doc, dict) else None
    if not isinstance(srv, dict) or not srv.get("url"):
        return
    hdrs = srv.get("headers") or {}
    lines = []
    for k, v in hdrs.items():
        if k in ("Authorization", "X-Coord-Mcp-Proxy-Key") and isinstance(v, str) and "\n" not in v:
            try:
                v = expand_env_ref_for_url(v, str(srv["url"]))
            except ValueError as e:
                # Not sent: an empty header file makes the caller fall through
                # to the bootstrap credential, and the reason is named.
                sys.stderr.write("machine-quiesce-check: coord-mcp proxy skipped: %s\n" % e)
                lines = []
                break
            lines.append("%s: %s\n" % (k, v))
    with open(hdr_out, "w") as fh:
        fh.write("".join(lines))
        # The transport-rung declaration, set EXPLICITLY (check #89): this
        # reader enumerates the whole headers block, and a runner that writes a
        # declaration INTO that block (the http arm's `native_mcp`) must never
        # have it ride along on a call this script makes over the loopback door.
        # Only beside a credential: an EMPTY file is how a refused nonce falls
        # through to the bootstrap, so a lone declaration must not fill it.
        if lines:
            fh.write("x-qontinui-transport: loopback_proxy\n")
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
# PER-RESOURCE VERDICTS (--for). Plan 2026-09-29-quiet-is-measured-by-session-
# existence-and-machine-wide-so-a-24x7-box-never-gets-one, Phase 3 and its
# section-2 table. Quiet is a property of a (task, repo) pair:
#   checkout-ff      blocked by a session WORKING (census activity working, or
#                    unknown -- which is never quiet) whose touches within the
#                    touch window name R, or include a SPECIFIC unattributed
#                    one (every repo), or are unattributed only GENERICALLY
#                    while R's own ref records (HEAD's log, its branch's log,
#                    the stash log; HEAD, refs/stash and the loose ref, else
#                    packed-refs, by mtime) moved inside the window, its tree
#                    has a tracked change or an untracked file written in it,
#                    or either cannot be read (a reader serving no specific_*
#                    reads any unattributed touch as every repo);
#                    a build reading
#                    R's working tree; a git lock in R; an AI session whose cwd
#                    names R. NOT by idle / waiting / stale sessions anywhere,
#                    nor by builds of other repos.
#   checkout-return  all of the above -- except that ANY unattributed touch by
#                    a working session in the window is every repo, unchanged
#                    -- PLUS any session not `finished` that touched R at any
#                    point in its life (its next commit would land on the
#                    wrong branch).
#   process-restart  today's machine-wide rule, unchanged, for every repo.
# UNKNOWN activity or UNKNOWN attribution is the blocking case, never quiet
# [policy: verification-and-evidence silent-empty-is-unknown].
PER_REPO_ONLY_PROBES = {"touch_ledger"}
# The machine-level probes a checkout-* verdict stands on. An unknown one makes
# every repo it has not already blocked UNKNOWN. coord_sessions is not among
# them (a checkout kills no session; for checkout-return an unreadable finished
# flag is folded in as "not finished", the blocking side), nor are custody and
# transcripts (the touch ledger replaced them).
CHECKOUT_PROBES = ("census", "runner_instances", "restart_readiness", "runner_coverage", "supervisor_builds")
BLOCKING_CAP = 20
ALL = None  # "every repo"


def _repo_of(path, root_native, primaries):
    """(repo, worktree) a path lands in, or (None, False). A linked worktree
    agent-worktrees/<id>/<repo>/... is (<repo>, True). The path is read as
    SPELLED first (its absolute form under the root's absolute form -- so a
    primary that is itself a symlink to a directory outside the root is still
    that primary), then RESOLVED (both sides realpath'd -- so a symlink from
    outside into a primary is that primary). Case is folded where the
    filesystem folds it, and the answer is spelled as the repo's own entry."""
    if not isinstance(path, str) or not path:
        return None, False
    p = path.replace("\\", "/")
    by_case = {os.path.normcase(r): r for r in primaries}
    for cand, rt in ((os.path.normcase(os.path.abspath(p)), os.path.normcase(os.path.abspath(root_native))),
                     (os.path.normcase(os.path.realpath(p)), os.path.normcase(os.path.realpath(root_native)))):
        if cand != rt and not cand.startswith(rt + os.sep):
            continue
        parts = os.path.relpath(cand, rt).replace("\\", "/").split("/")
        if parts[0] == os.path.normcase("agent-worktrees"):
            return (by_case.get(parts[2], parts[2]), True) if len(parts) >= 3 else (None, False)
        if by_case.get(parts[0]):
            return by_case[parts[0]], False
    return None, False


_DEP_CACHE = {}


def path_dep_repos(repo, root_native, primaries):
    """The primaries a Cargo workspace in <root>/<repo> path-depends on (the
    runner's path dependency on its schemas sibling), transitively. A build in
    R reads those working trees too."""
    if repo in _DEP_CACHE:
        return _DEP_CACHE[repo]
    _DEP_CACHE[repo] = set()
    out, base = set(), os.path.join(root_native, repo)
    tomls = []
    for pat in ("Cargo.toml", "*/Cargo.toml", "*/*/Cargo.toml", "*/*/*/Cargo.toml"):
        tomls += [t for t in glob.glob(os.path.join(base, pat)) if "node_modules" not in t and "/target" not in t.replace("\\", "/")]
    for t in tomls:
        try:
            with open(t, encoding="utf-8", errors="replace") as fh:
                txt = fh.read()
        except OSError:
            continue
        for m in re.finditer(r'\bpath\s*=\s*"([^"]+)"', txt):
            r, wt = _repo_of(os.path.join(os.path.dirname(t), m.group(1)), root_native, primaries)
            if r and r != repo and not wt:
                out.add(r)
    for r in list(out):
        out |= path_dep_repos(r, root_native, primaries)
    out.discard(repo)
    _DEP_CACHE[repo] = out
    return out


def build_repos(ba, root_native, primaries):
    """(repos-or-ALL, why) a build blocks. A build blocks the PRIMARY working
    trees it reads: its source checkout when that is a primary, and every
    primary its path dependencies land in. Its source is its cwd (Linux) or an
    absolute --manifest-path; a build sourced in a linked worktree reads none
    of a primary's files and blocks none (its target dir, which may sit inside
    a primary, is untracked and no checkout move touches it). Only a build
    whose source cannot be read falls back to its target dir -- blocking the
    repo that owns it, i.e. every repo sharing that target -- and one with no
    readable target either blocks every repo."""
    if not isinstance(ba, dict):
        return ALL, "no build attribution in the census row (a census predating it)"
    cwd, man, tgt = ba.get("cwd"), ba.get("manifest_path"), ba.get("target_dir")
    src = None
    if isinstance(man, str) and man:
        src = man if os.path.isabs(man) else (os.path.join(cwd, man) if cwd else None)
    if src is None and cwd:
        src = cwd
    if src:
        r, wt = _repo_of(src, root_native, primaries)
        if wt:
            return set(), "sourced in a linked worktree of %s (%s): reads no primary working tree" % (r, src)
        if r:
            return {r} | path_dep_repos(r, root_native, primaries), "reads %s (%s)" % (r, src)
        if _under_root(src, root_native):
            return ALL, "sourced at %s, which names no checkout" % src
        return set(), "sourced outside the workspace root (%s)" % src
    if isinstance(tgt, str) and tgt:
        if not os.path.isabs(tgt):
            # A relative target resolves against the BUILD's cwd, which is the
            # thing that could not be read -- never against this checker's own.
            return ALL, "source unreadable and target dir %r is relative to it" % tgt
        r, _wt = _repo_of(tgt, root_native, primaries)
        if r:
            return {r} | path_dep_repos(r, root_native, primaries), "source unreadable; target dir %s is %s's, so it blocks the repos sharing it" % (tgt, r)
        return ALL, "source unreadable and target dir %s names no checkout" % tgt
    return ALL, "neither its source nor its target dir could be read"


def _under_root(path, root_native):
    """Under the root as SPELLED or as RESOLVED. A path under the root that
    names no checkout -- including one whose realpath leaves the root -- is
    therefore unattributable (every repo), never "outside"."""
    for ap, rt in ((os.path.normcase(os.path.abspath(path)), os.path.normcase(os.path.abspath(root_native))),
                   (os.path.normcase(os.path.realpath(path)), os.path.normcase(os.path.realpath(root_native)))):
        if ap == rt or ap.startswith(rt + os.sep):
            return True
    return False


def _git(repo_dir, *args, raw=False):
    """git's stdout, stripped; None when git fails or times out. raw=True gives
    the BYTES unstripped -- `status -z` begins with a status column that may be
    a space, and a path is not text. Every call READS a shared checkout, so it
    takes no optional lock (--no-optional-locks, and GIT_OPTIONAL_LOCKS=0 for
    any git it runs) and consults no file-system monitor (core.fsmonitor=false):
    a configured hook would answer git status from its own state, and the
    builtin daemon (core.fsmonitor=true) is STARTED by a git status that finds
    it not running."""
    env = {k: v for k, v in os.environ.items() if k not in ("GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_INDEX_FILE")}
    env["GIT_OPTIONAL_LOCKS"] = "0"
    try:
        p = subprocess.run(["git", "--no-optional-locks", "-C", repo_dir, "-c", "core.fsmonitor=false"] + list(args),
                           capture_output=True, text=not raw, timeout=30, env=env)
    except (OSError, subprocess.SubprocessError):
        return None
    if p.returncode != 0:
        return None
    return p.stdout if raw else p.stdout.strip()


# One reflog line, matched over the WHOLE line and about as strict as git's own
# reader (show_one_reflog_ent in refs/files-backend.c, as of git v2.47.0) -- it
# also takes a pair of object ids that mixes 40 and 64 hex, which git would
# refuse, and it skips a blank line: two object ids, the identity up to the
# FIRST '>' after them (a name may hold a TAB), a space, the entry's time, a
# space and a +/-HHMM zone, then an optional TAB and message (`symbolic-ref`
# without -m writes none). The time is 1-20 ASCII digits. [0-9], never \d: \d
# also takes another script's decimal digits (U+0663, U+FF13), which int()
# reads too (1, U+0663, 3 reads as 133), so a line git refuses would be read as
# a time; and never str.isdigit(), which takes a superscript digit (U+00B2)
# that int() refuses -- a crash. At most 20, because int() also refuses a
# string past sys.get_int_max_str_digits().
_REFLOG_LINE = re.compile(r"\A(?:[0-9a-fA-F]{40}|[0-9a-fA-F]{64}) (?:[0-9a-fA-F]{40}|[0-9a-fA-F]{64}) "
                          r"[^>]*> ([0-9]{1,20}) [+-][0-9]{4}(?:\t.*)?\Z", re.S)
_OBJECT_ID = re.compile(r"\A(?:[0-9a-fA-F]{40}|[0-9a-fA-F]{64})\Z")
# A symref target refuses control characters, a space and DEL, as git's own
# ref-name check does: a NUL would otherwise reach subprocess as an argument.
_HEAD_SYMREF = re.compile(r"\Aref:\s*(refs/[^\x00-\x20\x7f]+)\Z")


def _reflog_newest(path, missing_ok):
    """The newest time one reflog FILE vouches for: the later of its newest
    entry's time and the file's mtime. 0 for a missing file when `missing_ok`
    (the stash log: no stash was ever made, or the last was dropped). None --
    UNKNOWN -- for a missing HEAD or branch log, a file that cannot be read, a
    torn last line, any line git's own reader would refuse (_REFLOG_LINE, or a
    time of 0), and a HEAD or branch log holding no entry at all
    (`git reflog expire --expire=now --all` leaves one 0 bytes long, hiding
    every move it held).

    The FILE, never `git log -g`: when HEAD's own log is missing or empty, git's
    reflog walk falls back to the BRANCH's log (the review-arm corroborator's
    `_head_log()` reads the file for the same reason). An entry's time is the
    committer date git stamped it with -- now, unless an exported
    GIT_COMMITTER_DATE says otherwise, which is why the mtime is read beside it
    -- and never the commit's own date: a `reset --hard` to an old commit is a
    recent entry."""
    try:
        with open(path, "rb") as fh:
            mtime = os.fstat(fh.fileno()).st_mtime
            text = fh.read().decode("utf-8", "replace")
    except FileNotFoundError:
        return 0 if missing_ok else None
    except OSError:
        return None
    lines = text.split("\n")
    if lines[-1].strip():
        return None  # a torn last line: git appends whole lines, so a missing newline is a cut-short write
    epochs = []
    for ln in lines[:-1]:
        if not ln.strip():
            continue
        m = _REFLOG_LINE.match(ln)
        if m is None or int(m.group(1)) == 0:
            return None  # a line git's own reader would refuse is UNKNOWN, never skipped
        epochs.append(int(m.group(1)))
    if not epochs and not missing_ok:
        return None  # a HEAD or branch log holding no entry was emptied, so it cannot say
    newest = max(epochs) if epochs else 0
    return max(newest, mtime)


def _mtime(path):
    """A file's mtime; 0 when there is no such file (a branch whose ref is
    packed has no loose one); None -- UNKNOWN -- when it cannot be read."""
    try:
        return os.stat(path).st_mtime
    except FileNotFoundError:
        return 0
    except OSError:
        return None


def _latest(*times):
    """The latest of several times; None -- UNKNOWN -- when any is None."""
    return None if None in times else max(times)


def _dir_above(path, stop):
    """The mtime of the nearest existing directory above `path`, no higher than
    `stop`: unlinking an entry rewrites it, so a loose ref deleted over an older
    packed value still leaves a trace. None when that cannot be read."""
    p = os.path.dirname(path)
    while True:
        try:
            return os.stat(p).st_mtime
        except FileNotFoundError:
            if os.path.normcase(os.path.abspath(p)) == os.path.normcase(os.path.abspath(stop)):
                return None
            p = os.path.dirname(p)
        except OSError:
            return None


def _changed(st):
    """When a WORKING-TREE file last changed: the later of its mtime and its
    ctime. A write that keeps or backdates the mtime -- cp -p, rsync -a, tar x,
    touch -d, an mv of an older file over it, a chmod -- still moves the ctime,
    which nothing in user space can set. That is POSIX's ctime: on Windows
    st_ctime is the CREATION time, which none of those writes moves, so nothing
    here could date them -- _tree_changes_written() dates no change there
    (any change blocks) and never reaches this. Not used for the .git
    internals: a fixture -- or anything -- that ages their mtime cannot age a
    ctime, and their entries already carry their own times."""
    return max(st.st_mtime, st.st_ctime)


def _wt_changed(path):
    """_changed() of one file by path; 0 when there is no such file; None --
    UNKNOWN -- when it cannot be read."""
    try:
        return _changed(os.stat(path))
    except FileNotFoundError:
        return 0
    except OSError:
        return None


def _named_git_dir(g):
    """The git dir as an explicit argument, for every git call that reads a
    primary's records: NAMED, never discovered -- from a primary whose .git is
    corrupt, discovery walks up into an enclosing repository (a workspace root
    may be one) and answers with that repository's records and tree."""
    return "--git-dir=" + g


def _primary_records(d):
    """The times one primary's ref records vouch for (primary_generic_blockers()),
    each None when that record cannot be read; None outright when HEAD names
    neither a branch nor an object id, or rev-parse cannot resolve the paths."""
    g = os.path.join(d, ".git")
    try:
        with open(os.path.join(g, "HEAD"), "rb") as fh:
            head_mtime = os.fstat(fh.fileno()).st_mtime
            head = fh.read().decode("utf-8", "replace").strip()
    except OSError:
        return None
    sym = _HEAD_SYMREF.match(head)
    if sym and ".." not in sym.group(1):
        ref = sym.group(1)
    elif _OBJECT_ID.match(head):
        ref = None  # a detached HEAD names no branch, so it has no branch log to read
    else:
        return None
    q = [_named_git_dir(g), "rev-parse", "--path-format=absolute", "--git-path", "logs/HEAD",
         "--git-path", "logs/refs/stash"]
    if ref:
        q += ["--git-path", "logs/" + ref, "--git-path", ref, "--git-path", "packed-refs"]
    q += ["--git-path", "refs/stash"]  # last, so every path above keeps its place
    out = _git(d, *q)
    if out is None:  # one retry: a transient failure on a loaded box is not blindness
        out = _git(d, *q)
    paths = (out or "").splitlines()
    if len(paths) != (6 if ref else 3):
        return None
    times = [head_mtime,  # HEAD itself: a tool that rewrites HEAD without logging leaves only this
             _reflog_newest(paths[0], False),  # logs/HEAD
             _reflog_newest(paths[1], True)]  # logs/refs/stash
    # The loose refs/stash: `git update-ref refs/stash` logs nothing when refs/stash has no log yet,
    # unless --create-reflog (or core.logAllRefUpdates=always), so only this file's mtime records it.
    times.append(_mtime(paths[-1]))  # the loose refs/stash
    if ref:
        times.append(_reflog_newest(paths[2], False))  # the branch's own log: a move made from another worktree skips logs/HEAD
        loose = _mtime(paths[3])  # the branch's loose ref, when one exists: a ref file written with no log entry
        if loose == 0:  # no loose ref: packed-refs (a packed ref rewritten in place), and the directory a loose ref was unlinked from
            loose = _latest(_mtime(paths[4]), _dir_above(paths[3], g))
        times.append(loose)
    return times


def _path_written(path, root):
    """When one dirty path was last changed (_changed: mtime or ctime): its own
    (lstat: a tracked symlink is itself the change); for a path that is gone,
    its nearest existing parent directory's (removing an entry rewrites its
    parent); None -- UNKNOWN -- when that cannot be read, or the checkout root
    itself is gone."""
    p = path
    while True:
        try:
            return _changed(os.lstat(p))
        except FileNotFoundError:
            if os.path.normcase(os.path.abspath(p)) == os.path.normcase(os.path.abspath(root)):
                return None
            p = os.path.dirname(p)  # a path that is gone: its nearest existing parent records the removal
        except OSError:
            return None


def _tree_changes_written(d, since):
    """Whether a primary's working tree carries a change written inside the
    window: a TRACKED change, or an UNTRACKED file -- git status
    --untracked-files=all lists every one, and never an ignored file, which no
    residue restore touches. False when the tree is clean, or (POSIX) every
    change in it is older -- an old dirty tree stays QUIET, untracked files
    included, so a stale residue can still be restored or removed. None --
    UNKNOWN -- when git status fails or times out, or a dirty path cannot be
    read. A change is dated by the latest of the index's change time (staging
    writes it), each dirty path's own (both paths of a rename or copy; an
    untracked file's exactly as a tracked one's) and, for a path that is gone,
    its nearest existing parent's -- each by mtime or ctime (_changed). That
    dating is POSIX's: on Windows (os.name nt) st_ctime is the CREATION time,
    so cp -p or an mv of an older file over a tracked path, or an mv of one
    into an untracked path, leaves nothing that could date the write, and ANY
    change there -- an untracked file included -- reads as written inside the
    window: a change that cannot be dated is never read as old. Why it counts:
    checkout-ff QUIET also gates --restore-residue, which reverts tracked files
    to a classed upstream state AND unlinks each untracked file byte-identical
    to the upstream tip, even in a tree with no tracked change -- so a change a
    live session wrote -- `GIT_DIR=$R/.git git checkout <old> -- p`,
    `sed -i ... $C/x.md`, `cp $W/x $R/x` of the tip's x,
    `git --git-dir=$R/.git show origin/main:x > $R/x` -- would be reverted or
    unlinked under it. Every untracked file counts, not only one at a path the
    last-fetched origin/<default> has: the sweep fetches before it restores,
    so that tip cannot bound which ones a restore unlinks. _git() runs this
    status with --no-optional-locks (and GIT_OPTIONAL_LOCKS=0), so it never
    takes index.lock in a shared checkout -- a live session's commit would
    fail on it -- nor rewrites the index whose time it then reads, and with
    core.fsmonitor=false, so it scans the tree itself rather than trusting a
    monitor's own state."""
    g = os.path.join(d, ".git")
    q = (_named_git_dir(g), "--work-tree=" + d, "status", "--porcelain=v1", "-z", "--untracked-files=all")
    out = _git(d, *q, raw=True)
    if out is None:  # one retry: a transient failure on a loaded box is not blindness
        out = _git(d, *q, raw=True)
    if out is None:
        return None  # a status that failed or timed out cannot say the tree is clean
    fields, paths, i = out.split(b"\0"), [], 0
    while i < len(fields):
        entry, i = fields[i], i + 1
        if not entry:
            continue
        if len(entry) < 4:
            return None  # a status entry out of shape
        paths.append(entry[3:])  # `XY path`, a tracked change; `?? path`, an untracked file -- dated alike below
        if (b"R" in entry[:2] or b"C" in entry[:2]) and i < len(fields):
            paths.append(fields[i])  # a rename or copy names its source in the next field
            i += 1
    if not paths:
        return False
    if os.name == "nt":
        return True  # st_ctime is the CREATION time here, so no time can date a write that kept its mtime: any change blocks, an untracked file included
    times = [_wt_changed(os.path.join(g, "index"))]  # staging (git add, rm --cached, checkout -- p) rewrites the index
    times += [_path_written(os.path.join(d, os.fsdecode(p)), d) for p in paths]
    if None in times:
        return None
    return max(times) >= since


def primary_generic_blockers(primaries, root_native, since):
    """(moved, dirty, unread) over the primaries: what a working session whose
    in-window unattributed touches are all GENERIC blocks under checkout-ff.

    MOVED: a record git keeps of a ref write is newer than `since` -- the
    primary's own logs/HEAD, the log of the branch HEAD names, logs/refs/stash
    (each by its newest entry's time or the file's mtime, whichever is later),
    or the mtime of HEAD itself, of the loose refs/stash, of that branch's loose
    ref, or, with no loose ref, of packed-refs and of the directory a loose ref
    was unlinked from (HEAD rewritten by a tool that does not log, a ref file
    written directly -- a stash made by `update-ref refs/stash` among them -- a
    packed ref rewritten in place, a loose ref deleted over an older packed
    value). Residual, stated: these .git internals are read by mtime and
    entries only, so one rewritten with a preserved or backdated mtime and no
    log entry is not seen.

    DIRTY: the working tree carries a change written inside the window -- a
    tracked change or an untracked file, never an ignored one
    (_tree_changes_written) -- by mtime or ctime; on Windows, where st_ctime
    is the creation time, any change, an untracked file included.

    UNREAD -- UNKNOWN, never "no move" [policy: verification-and-evidence
    unknown-must-not-render-as-a-default]: HEAD names neither a branch nor an
    object id, rev-parse cannot resolve the records' paths, a record cannot be
    read (_reflog_newest(); a branch log is required on a branch, and a
    detached HEAD has none), git status fails, or reading the primary raised at
    all -- an input no reader here anticipated is that primary's UNKNOWN, named
    on stderr, never the whole probe's crash. The git dir is NAMED rather than
    discovered (_named_git_dir).

    Cost, per primary: one read of HEAD and one rev-parse, and a git status only
    when those leave it undecided (not moved, not unreadable) -- each retried
    once, so one to four git calls -- at _git()'s 30 s timeout, plus a few
    small file reads and one lstat per dirty path, untracked files included
    (that status walks every directory that is not ignored). Nothing bounds
    the whole run to the scheduler's 120 s probe ceiling
    (schedule-return-to-main.sh PROBE_TIMEOUT_SECONDS): a probe that runs past
    it is killed and reads NOT met (the runner's scheduler_probe.rs), so the
    job does not fire -- the safe direction."""
    moved, dirty, unread = set(), set(), set()
    for r in sorted(primaries):
        try:
            verdict = _primary_verdict(os.path.join(root_native, r), since)
        except Exception as e:  # one primary's unanticipated input is its own UNKNOWN, never the whole probe's crash
            sys.stderr.write("machine-quiesce-check: reading %s's ref records raised %s (%s) -- read as UNKNOWN\n"
                             % (r, type(e).__name__, e))
            verdict = None
        if verdict is None:
            unread.add(r)
        elif verdict == "moved":
            moved.add(r)
        elif verdict == "dirty":
            dirty.add(r)
    return moved, dirty, unread


def _primary_verdict(d, since):
    """One primary's verdict -- moved, dirty, still, or None (UNKNOWN): its ref
    records first, then its git status only when they leave it undecided -- a
    status costs a git call per primary, and a moved or unreadable primary
    blocks whatever its tree holds."""
    times = _primary_records(d)
    if times is None or None in times:
        return None
    if max(times) >= since:
        return "moved"
    written = _tree_changes_written(d, since)
    if written is None:
        return None
    return "dirty" if written else "still"


def _names(repos, cap=6):
    """A repo set spelled for a one-line `why`: the names, or a count and the
    first few when the set is long."""
    s = sorted(repos)
    if not s:
        return "none"
    return ",".join(s) if len(s) <= cap else "%d: %s,..." % (len(s), ",".join(s[:cap]))


def repo_work(for_class, repo_dir):
    """Whether R has something the sweep would do under this task class, read
    cheaply from what is already on disk (NO fetch: origin/<default> as last
    fetched -- the sweep fetches before it acts). checkout-ff: on its default
    branch and behind origin/<default>, or modified TRACKED files (residue
    candidates). An untracked file alone is not, although a residue restore
    does unlink one: only one byte-identical to the upstream tip's file at its
    own path (dirty-provenance.sh untracked_on_upstream). On the default
    branch level with origin/<default> that path is one HEAD tracks, so an
    untracked file there means the index dropped it -- a staged deletion,
    which this --untracked-files=no status lists -- and behind origin the
    behind-count is work already; counting every untracked file would fire
    the job for files no restore can touch. The residual, stated: a parked or
    ahead checkout whose only dirt is such a file reads "nothing to do" and
    fires nothing by itself -- a delay, never a move: the sweep still visits
    it on a run another repo fires. Whether a restore may run at all is
    per_repo_verdicts()'s question, not this one, and its GENERIC arm reads
    untracked files (_tree_changes_written).
    checkout-return: HEAD on another branch or detached. A read that fails is
    reported as work (`unknown`): an unproven "nothing to do" must not keep
    the job from firing."""
    default = _git(repo_dir, "symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD")
    default = default[len("origin/"):] if default and default.startswith("origin/") else None
    if not default:
        for b in ("main", "master"):
            if _git(repo_dir, "rev-parse", "--verify", "--quiet", "refs/remotes/origin/" + b) is not None:
                default = b
                break
    if not default:
        return True, "unknown: no origin default branch could be read"
    head = _git(repo_dir, "symbolic-ref", "--quiet", "--short", "HEAD")
    if for_class == "checkout-return":
        if head == default:
            return False, "on %s" % default
        return True, "parked on %s, default %s" % (head or "<detached HEAD>", default)
    if head == default:
        n = _git(repo_dir, "rev-list", "--count", "HEAD..refs/remotes/origin/" + default)
        if n is None or not n.isdigit():
            return True, "unknown: behind-count on %s unreadable" % default
        if int(n) > 0:
            return True, "%s commit(s) behind origin/%s (as last fetched)" % (n, default)
    st = _git(repo_dir, "status", "--porcelain", "--untracked-files=no")
    if st is None:
        return True, "unknown: git status failed"
    if st:
        return True, "%d modified tracked file(s) (residue candidates)" % len(st.splitlines())
    return False, ("level with origin/%s, clean" % default) if head == default else ("parked on %s (a checkout-return matter), clean" % (head or "<detached>"))


def per_repo_verdicts(for_class, machine_verdict, per_repo, census, blocking, probes, rd_items, finished_sids, coord_ok,
                      root_native, touch_window_s, exit_quiet, ledger_path, ledger_rc, probe):
    primaries = set(per_repo)
    targets = []  # (repos-or-ALL, entry)

    if for_class == "process-restart":
        # Today's machine-wide rule, unchanged: every hosted non-finished
        # session, every build, every active coord row blocks every repo, and
        # the machine verdict (overridable-only BUSY included) is every repo's.
        for e in blocking:
            targets.append((ALL, dict(e)))
    else:
        # ---- the touch ledger ---------------------------------------------
        led, led_state = None, "ok"
        if ledger_rc == "3":
            led_state = "no ledger directory on this machine (the touch-ledger hook has never recorded here)"
        elif ledger_rc == "":
            led_state = "the ledger reader (touch-ledger-read.sh) was not found"
        elif ledger_rc != "0":
            led_state = "the ledger reader exited %s" % ledger_rc
        else:
            led = load(ledger_path)
            if not isinstance(led, dict) or not isinstance(led.get("sessions"), dict):
                led, led_state = None, "the ledger reader's output is unparseable"
        if led is None:
            probe("touch_ledger", "unknown", "%s -- every session is UNATTRIBUTED, so a working one blocks every repo" % led_state)
        else:
            probe("touch_ledger", "ok", "%d ledger file(s), %d session(s) recorded, %d live session(s) with no ledger (read as unattributed), %d truncated" % (
                led.get("ledgers") or 0, len(led["sessions"]), len(led.get("no_ledger") or []), len(led.get("truncated") or [])))
        no_ledger = set(led.get("no_ledger") or []) if led else set()
        by_case = {os.path.normcase(r): r for r in primaries}

        def canon(r):
            # A ledger written on a case-insensitive root may spell a repo
            # differently from its directory entry; compare as the OS does.
            return by_case.get(os.path.normcase(r), r)
        since = NOW - touch_window_s
        generic_read = []  # primary_generic_blockers()'s one answer for this run

        def generic_blockers():
            """(moved, dirty, unread) -- read only when some working session's
            in-window unattributed lines are all GENERIC, and at most once per run."""
            if not generic_read:
                generic_read.append(primary_generic_blockers(primaries, root_native, since))
            return generic_read[0]

        def attribution(sid, lifetime):
            """(repos-or-ALL, why). Window touches for a working session;
            whole-life PRIMARY touches for checkout-return's non-finished rule."""
            if led is None:
                return ALL, "attribution UNKNOWN: " + led_state
            if not sid:
                return ALL, "attribution UNKNOWN: no session id for this process"
            if sid in no_ledger or sid not in led["sessions"]:
                return ALL, "attribution UNKNOWN: this session has no touch ledger (started before the hook, or no mutating call yet)"
            L = led["sessions"][sid]
            u = L.get("unattributed") or {}
            repos = set()
            if lifetime:
                if L.get("truncated"):
                    return ALL, "whole-life attribution UNKNOWN: the ledger was truncated at its rotation cap"
                if (u.get("specific_count") or 0) > 0:
                    return ALL, ("changed directory to somewhere the ledger cannot name at some point (an unresolvable "
                                 "cd / -C / --manifest-path / --prefix / --directory / --cwd target)")
                for r, v in (L.get("repos") or {}).items():
                    if (v.get("count") or 0) > 0:  # primary touches only: a worktree commit never lands on R's branch
                        repos.add(canon(r))
                return repos, "touched %s in its life" % (",".join(sorted(repos)) or "no primary")
            for r, v in (L.get("repos") or {}).items():
                if max(v.get("last_epoch") or 0, v.get("worktree_last_epoch") or 0) >= since:
                    repos.add(canon(r))
            why = "touched %s within %ds" % (",".join(sorted(repos)) or "no repo", touch_window_s)
            if (u.get("last_epoch") or 0) < since:
                return repos, why
            # An unattributed mutating call inside the window. Every class but
            # checkout-ff keeps reading it as every repo (a return switches
            # branches, and the lifetime rule is already blind to a switch that
            # landed GENERIC), and so does a reader that serves no specific_*:
            # it cannot tell GENERIC from SPECIFIC, and absent is not null.
            if for_class != "checkout-ff" or "specific_last_epoch" not in u:
                return ALL, "an unattributed mutating call within %ds" % touch_window_s
            if (u.get("specific_last_epoch") or 0) >= since:
                return ALL, ("a SPECIFIC unattributed mutating call within %ds: it names somewhere the ledger cannot "
                             "(an unresolvable cd / -C / --manifest-path / --prefix / --directory / --cwd target, an "
                             "Edit/Write path it cannot resolve, an unreadable envelope or an unresolved workspace "
                             "root)" % touch_window_s)
            # GENERIC lines only (the no-path marker, a "generic":true `$`
            # argument). One can still hide a ref move -- GIT_DIR=, --git-dir=,
            # -c core.worktree=, xargs {}, a script -- or a write into the
            # tree, tracked or untracked, so each repo's own records decide
            # (primary_generic_blockers): HEAD's log, the named branch's log
            # and the stash log, the mtimes of those files, of HEAD, of
            # refs/stash and of the branch's loose ref (else packed-refs), and
            # the tree's changes written in the window, untracked files
            # included.
            moved, dirty, unread = generic_blockers()
            repos.update(moved, dirty, unread)
            return repos, ("%s; GENERIC unattributed calls within %ds, so also every repo whose HEAD, branch or stash "
                           "records moved in it (%s), has a tracked change or an untracked file written in it (%s), "
                           "or cannot be read (%s)"
                           % (why, touch_window_s, _names(moved), _names(dirty), _names(unread)))

        census_pids = set()
        for p in (census or {}).get("processes") or []:
            kind, origin = p.get("kind"), p.get("origin")
            if origin == "self":
                continue
            if kind in ("codex", "pi"):
                census_pids.add(p.get("pid"))
                targets.append((ALL, {"class": "external_agent_no_idle_signal", "source": "census", "pid": p.get("pid"), "kind": kind,
                                      "why": "no activity or attribution reader for this agent family"}))
                continue
            if kind != "claude":
                if kind == "build" and p.get("ide_check") is not True:
                    repos, why = build_repos(p.get("build_attribution"), root_native, primaries)
                    targets.append((repos, {"class": "build_process", "source": "census", "pid": p.get("pid"), "name": p.get("name"),
                                            "why": why}))
                continue
            census_pids.add(p.get("pid"))
            cls = "runner_session" if origin == "runner" else "external_session"
            sid, act = p.get("session_id"), p.get("activity") or "unknown"
            cwd_r, cwd_wt = _repo_of(p.get("cwd"), root_native, primaries)
            if cwd_r not in primaries:
                cwd_r = None
            base = {"class": cls, "source": "census", "pid": p.get("pid"), "session_id": sid, "activity": act}
            if act in ("working", "unknown"):
                repos, why = attribution(sid, False)
                if repos is not ALL and cwd_r:
                    repos = repos | {cwd_r}
                targets.append((repos, dict(base, rule="working", why="activity %s; %s" % (act, why))))
            if for_class == "checkout-return" and not (sid and sid in finished_sids):
                repos, why = attribution(sid, True)
                if repos is not ALL and cwd_r and not cwd_wt:
                    repos = repos | {cwd_r}
                targets.append((repos, dict(base, rule="lifetime", why="not finished%s; %s" % (
                    "" if coord_ok else " (coord unread, so finished is unknown)", why))))

        # ---- the runner's planes ------------------------------------------
        for kind, key, ent in rd_items:
            if kind == "pid":
                if key in census_pids:
                    continue  # judged above, by its own activity and ledger
                targets.append((ALL, dict(ent, why="listed by the runner but absent from the census, so its activity is unknown")))
            elif kind == "ai":
                # Plan section 2: an AI session blocks the repo its worktree or
                # cwd names (a worktree of R names R), else every repo.
                r, _wt = _repo_of(key, root_native, primaries)
                r = r if r in primaries else None
                targets.append(({r} if r else ALL, dict(ent, why=("its cwd names %s" % r) if r else "its cwd names no checkout")))
            else:
                targets.append((ALL, dict(ent, why="not itemised, so not attributable")))

        # ---- cargo-guard locks and supervisor slots -------------------------
        build_anc = set()
        for p in (census or {}).get("processes") or []:
            if p.get("kind") == "build" and p.get("origin") != "self":
                build_anc.add(p.get("pid"))
                for a in p.get("ancestry") or []:
                    m = re.search(r"\((\d+)\)$", a)
                    if m:
                        build_anc.add(int(m.group(1)))
        for e in blocking:
            if e.get("class") == "build_lock":
                r, wt = _repo_of(os.path.dirname(os.path.dirname(os.path.dirname(e["lockdir"]))), root_native, primaries)
                hp = e.get("holder_pid")
                hp = int(hp) if isinstance(hp, str) and hp.isdigit() else None
                if hp is not None and hp in build_anc:
                    continue  # its build processes are in the census and attributed there
                if wt or r is None or r not in primaries:
                    targets.append((set() if wt else ALL, dict(e, why="lock of a linked worktree's own target" if wt else "lockdir names no checkout")))
                else:
                    targets.append(({r} | path_dep_repos(r, root_native, primaries),
                                    dict(e, why="guards %s's shared target and its holder's build is not in the census" % r)))
            elif e.get("class") == "build_slot":
                r = "qontinui-runner"
                targets.append((({r} | path_dep_repos(r, root_native, primaries)) if r in primaries else ALL,
                                dict(e, why="the supervisor builds the runner")))

        # ---- git locks: a checkout move would fail or race here ------------
        for r in sorted(primaries):
            gd = os.path.join(root_native, r, ".git")
            for lk in ("index.lock", "HEAD.lock"):
                if os.path.exists(os.path.join(gd, lk)):
                    targets.append(({r}, {"class": "git_lock", "source": "filesystem", "lock": "%s/.git/%s" % (r, lk),
                                          "why": "a git process is mid-write in this checkout"}))

    # ---- fold into per-repo verdicts ------------------------------------------
    unk = [p for p in probes if p["status"] == "unknown" and p["name"].split(":")[0] in CHECKOUT_PROBES]
    counts = {"QUIET": 0, "BUSY": 0, "UNKNOWN": 0}
    for r in sorted(per_repo):
        hits = [e for repos, e in targets if repos is ALL or r in repos]
        v = per_repo[r]
        v["blocking_count"] = len(hits)
        v["blocking"] = hits[:BLOCKING_CAP]
        if len(hits) > BLOCKING_CAP:
            v["blocking_truncated"] = True
        if for_class == "process-restart":
            v["verdict"] = machine_verdict
        elif hits:
            v["verdict"] = "BUSY"
        elif unk:
            v["verdict"] = "UNKNOWN"
            v["unknown_because"] = "; ".join("%s: %s" % (p["name"], p["detail"][:120]) for p in unk[:3])
        else:
            v["verdict"] = "QUIET"
        counts[v["verdict"]] += 1

    rc = None
    if exit_quiet:
        quiet = [r for r in sorted(per_repo) if per_repo[r]["verdict"] == "QUIET"]
        rc = 5 if quiet else (1 if counts["BUSY"] else 3)
        for r in quiet:
            has, why = repo_work(for_class, os.path.join(root_native, r))
            per_repo[r]["work"] = {"has_work": has, "reason": why}
            if has:
                rc = 0
                break
    return {"counts": counts, "act_eligible": sorted(r for r in per_repo if per_repo[r]["verdict"] == "QUIET"),
            "touch_window_s": touch_window_s}, rc


# --------------------------------------------------------------------------
def cmd_final(work, census_path, census_rc, root, root_native, self_sid, device_id, script_dir, mode,
              for_class="", touch_window_s="21600", exit_quiet="0", ledger_path="", ledger_rc=""):
    probes, blocking, overridable, ide_ignored = [], [], [], []
    rd_items = []  # (repos-or-ALL, entry): readiness planes, itemised for --for checkout-*

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
                rd_items.append(("pid", pid, blocking[-1]))
            if residual:
                blocking.append({"class": "runner_session", "source": "restart-readiness", "port": port,
                                 "count": residual, "detail": "blocking processes the endpoint did not itemise"})
                rd_items.append(("all", None, blocking[-1]))
            if ai_left:
                blocking.append({"class": "ai_session", "source": "restart-readiness", "port": port, "count": ai_left})
                ai_other = [q for q in ai_procs if q.get("pid") not in self_pids]
                for q in ai_other[:ai_left]:
                    rd_items.append(("ai", q.get("cwd"), {"class": "ai_session", "source": "restart-readiness", "port": port,
                                                          "pid": q.get("pid"), "cwd": q.get("cwd")}))
                if ai_left > len(ai_other):
                    rd_items.append(("all", None, {"class": "ai_session", "source": "restart-readiness", "port": port,
                                                   "count": ai_left - len(ai_other),
                                                   "detail": "AI sessions the endpoint did not itemise"}))
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
                    rd_items.append(("all", None, blocking[-1]))
                if ac:
                    blocking.append({"class": "ai_session", "source": "restart-readiness", "port": port, "count": ac})
                    rd_items.append(("all", None, blocking[-1]))
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
    # Read the knobs by cargo-guard.sh's own rules (its LOCK_HEARTBEAT_INTERVAL
    # case and acquire_build_lock's lock-config-invalid check): a heartbeat that
    # is not 1-9999 falls back to 30; a STALE_AFTER that is not digits, has a
    # leading zero, runs past 9 digits, or is not above the heartbeat is one
    # cargo-guard refuses -- read here it would count a LIVE lock as stale
    # (negative, or between two heartbeats) and discount it into quiet.
    hb = os.environ.get("CARGO_GUARD_LOCK_HEARTBEAT_INTERVAL") or "30"
    hb = int(hb) if re.fullmatch(r"[1-9][0-9]{0,3}", hb) else 30
    stale_raw = os.environ.get("CARGO_GUARD_LOCK_STALE_AFTER") or "120"
    stale_ok = re.fullmatch(r"[1-9][0-9]{0,8}", stale_raw) is not None and int(stale_raw) > hb
    stale_after = int(stale_raw) if stale_ok else 0
    locks = sorted(set(glob.glob(os.path.join(root_native, "*", ".build-state", "locks", "cargo.lockdir")) +
                       glob.glob(os.path.join(root_native, "agent-worktrees", "*", "*", ".build-state", "locks", "cargo.lockdir"))))
    if not locks:
        probe("cargo_guard_lock", "not_applicable", "no cargo.lockdir under any checkout")
    elif not stale_ok:
        probe("cargo_guard_lock", "unknown",
              "CARGO_GUARD_LOCK_STALE_AFTER=%r is not a whole number of seconds above the %ds heartbeat "
              "(cargo-guard.sh refuses it: lock-config-invalid), so no lockdir's freshness can be read"
              % (stale_raw, hb))
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
    finished_sids, coord_ok = set(), False
    if for_class == "checkout-ff":
        probe("coord_sessions", "not_applicable", "--for checkout-ff does not read coord rows: a fast-forward kills no session, "
              "so neither an active row nor its finished flag bears on it")
    elif not device_id:
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
            coord_ok = True
            for r in rows:
                if r.get("deviceId") and r.get("deviceId") != device_id:
                    continue
                if r.get("state") != "active":
                    continue
                active += 1
                if r.get("sessionStatus") == "finished":
                    finished += 1
                    if r.get("claudeCodeSessionId"):
                        finished_sids.add(r["claudeCodeSessionId"])
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
        if for_class:
            # Replaced under --for by the touch ledger (per_repo[R].verdict):
            # the project-dir prefix guess read every repo as touched 0 s ago
            # on a box whose sessions all sit at the workspace root.
            for k in ("newest_transcript_write", "newest_transcript_age_s"):
                v.pop(k, None)
            continue
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
    unknown = any(p["status"] == "unknown" for p in probes if p["name"] not in PER_REPO_ONLY_PROBES)
    if blocking:
        verdict = "BUSY"
    elif unknown:
        verdict = "UNKNOWN"
    elif overridable:
        verdict = "BUSY"
    else:
        verdict = "QUIET"

    # ---- per-resource verdicts (--for) ------------------------------------------
    per_summary, exit_quiet_rc = None, None
    if for_class:
        per_summary, exit_quiet_rc = per_repo_verdicts(
            for_class, verdict, per_repo, census if isinstance(census, dict) else None, blocking, probes,
            rd_items, finished_sids, coord_ok, root_native, int(touch_window_s), exit_quiet == "1",
            ledger_path, ledger_rc, probe)
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
    if for_class:
        doc["for"] = for_class
        doc["per_repo_summary"] = per_summary
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
        if for_class:
            print("  per repo, --for %s (%s):" % (for_class, ", ".join("%s %d" % kv for kv in sorted(per_summary["counts"].items()))))
            for r, v in per_repo.items():
                why = ("%d blocking, first %s: %s" % (v["blocking_count"], v["blocking"][0]["class"], v["blocking"][0].get("why", ""))
                       if v["blocking"] else (v.get("unknown_because") or ""))
                work = v.get("work")
                print("    %-36s %-8s %s%s" % (r, v["verdict"], why[:180], ("  work: " + work["reason"]) if work else ""))
        else:
            print("  per repo (custody age / transcript age):")
            for r, v in per_repo.items():
                fmt = lambda s: "-" if s is None else ("%dh%02dm" % (s // 3600, (s % 3600) // 60))
                print("    %-36s %-9s %s" % (r, fmt(v["last_custody_age_s"]), fmt(v["newest_transcript_age_s"])))
    if exit_quiet_rc is not None:
        return exit_quiet_rc
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
    elif sub == "census-sids":
        c = load(sys.argv[2])
        print(",".join(sorted({p["session_id"] for p in ((c or {}).get("processes") or []) if isinstance(p, dict)
                               and isinstance(p.get("session_id"), str) and re.match(r"^[A-Za-z0-9_-]+$", p["session_id"])
                               and p.get("origin") != "self"})) if isinstance(c, dict) else "")
    elif sub == "final":
        sys.exit(cmd_final(*sys.argv[2:16]))
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
# A fast-forward kills no session, so --for checkout-ff reads no coord row (and
# mints no credential every time the scheduler polls it).
if [ -n "$DEVICE_ID" ] && [ "$FOR" != checkout-ff ]; then
  PROXY_OK=0
  if [ -f "$ROOT/.mcp.json" ]; then
    PROXY_BASE="$(QMQC_PY_LIB="$(native "$LIB_DIR")" qpy proxy "$(native "$ROOT/.mcp.json")" "$(native "$WORK/proxy.hdr")")"
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

# ---- 4. the touch ledger (--for checkout-*) ------------------------------------
# One whole-history read, keyed by session, naming every census session so a
# live session with NO ledger is reported rather than read as touching nothing.
LEDGER_RC=""
if [ "$FOR" = checkout-ff ] || [ "$FOR" = checkout-return ]; then
  LEDGER_READ="${QMQC_TOUCH_LEDGER_READ:-}"
  if [ -z "$LEDGER_READ" ]; then
    for c in "$SCRIPT_DIR/touch-ledger-read.sh" "$ROOT/qontinui-claude-config/scripts/touch-ledger-read.sh" \
             "$SCRIPT_DIR/../../../scripts/touch-ledger-read.sh"; do
      [ -f "$c" ] && { LEDGER_READ="$c"; break; }
    done
  fi
  if [ -n "$LEDGER_READ" ]; then
    SIDS="$(qpy census-sids "$(native "$WORK/census.json")")"
    bash "$LEDGER_READ" --by-session --sessions "$SIDS" >"$WORK/ledger.json" 2>"$WORK/ledger.err"; LEDGER_RC=$?
  fi
fi

# ---- 5. aggregate --------------------------------------------------------------
MODE=text; [ "$JSON" = 1 ] && MODE=json
qpy final "$(native "$WORK")" "$(native "$WORK/census.json")" "$CENSUS_RC" "$ROOT" "$(native "$ROOT")" \
  "$SELF_SID" "$DEVICE_ID" "$(native "$SCRIPT_DIR")" "$MODE" \
  "$FOR" "$TOUCH_WINDOW_S" "$EXIT_QUIET" "$(native "$WORK/ledger.json")" "$LEDGER_RC"
exit $?
