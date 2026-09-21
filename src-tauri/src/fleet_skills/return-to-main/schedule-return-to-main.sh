#!/usr/bin/env bash
# schedule-return-to-main.sh - register (or check / remove) the nightly
# /return-to-main session as ONE task, named `return-to-main`, in the RUNNER'S
# OWN SCHEDULER.
#
# Plan: 2026-09-13-nightly-return-to-main-sweep (Phase 5c).
#
# ascii-only-source
#
# -- THE ONLY SCHEDULING MECHANISM FOR THIS JOB -------------------------------
# Operator ruling 2026-09-13: scheduling happens INSIDE Qontinui -- in the
# runner, or spawned by coord -- never as a Windows Task Scheduler job, a
# systemd timer or a cron entry. Users run Windows, macOS and Linux, and nobody
# should need an external system to use Qontinui. So this helper is the ONE way
# the nightly job is registered, and it registers NOTHING with the operating
# system: it upserts a task in the runner's scheduler (persisted in the
# runner's Postgres, evaluated by its 60 s tick, a missed slot caught up once
# after a runner start) through the runner's HTTP API at 127.0.0.1:9876.
# Because the runner owns the schedule, this works identically on Windows,
# macOS and Linux -- bash, curl and a running primary runner are the whole
# dependency list. The systemd/crontab installer it replaces
# (install-return-to-main-sweep.sh) is deleted, not kept as an alternative.
#
# The task is visible and editable in the runner's Scheduler UI. An edit made
# there is DRIFT: --check names it, and the next --install puts the intended
# body back.
#
# -- WHAT IT REGISTERS --------------------------------------------------------
# API: runner src-tauri/src/mcp/scheduler.rs (routes /scheduler/tasks[/{id}]);
# shapes: qontinui-schemas rust/src/scheduler.rs (ScheduleExpression,
# ScheduledTaskType::RemoteAgent, CatchUpPolicy).
#   name              return-to-main   -- the UPSERT KEY. The runner has no
#                     unique name, so the helper finds the task by name through
#                     GET /scheduler/tasks and refuses (exit 3) when two exist.
#   schedule          {"type":"Cron","value":"<MM> <HH> * * *"} from --at
#                     (default 04:20). The runner prepends the seconds field of
#                     a 5-field cron itself (scheduler.rs compute_next_run).
#   task              {"task_type":"RemoteAgent", prompt, working_directory,
#                     max_turns 200, timeout_seconds 3600}. The runner's own
#                     defaults (50 turns, 600 s) apply only when these are unset,
#                     and a sweep plus adjudication does not fit in them.
#   prompt            /return-to-main --shadow --not-after 06:30  (default)
#                     /return-to-main --act --not-after 06:30     (--act)
#                     The skill runs in SHADOW unless --act is passed, so arming
#                     the job means the prompt CONTAINS --act; omitting --shadow
#                     alone would leave it in shadow.
#   working_directory <workspace-root>, in the native (`cygpath -w`, backslash)
#                     spelling on Windows, because the runner hands it to a
#                     native process.
#   catchUpPolicy     run_once, stated explicitly although it is the default: a
#                     runner that was down at 04:20 fires ONE late run at start,
#                     and --not-after is what bounds that late fire.
#   skipIfCompleted   false and autoFixOnFailure false, stated explicitly -- a
#                     skip-after-first-success task would run exactly once.
#
# -- TIME ZONE: WHAT TZ_DRIFT MEANS -------------------------------------------
# A runner build predating plan Phase 5a evaluates cron in UTC (compute_next_run
# and the missed-run enumerator both work in Utc; the scheduler's `timezone`
# setting is stored and never read). Measured 2026-09-13 on a UTC+2 box: cron
# `20 4 * * *` -> nextRun 04:20Z = 06:20 local, two hours late. --check converts
# the task's nextRun into THIS machine's local time (as `date` sees it) and
# compares it with the hour and minute in the task's own cron; a mismatch prints
# TZ_DRIFT. The helper deliberately does NOT re-express the cron in UTC to
# compensate: that would fire at the wrong time the day 5a lands, and would be
# off by an hour for half of every year across DST.
#
# -- WHERE THE MACHINERY LIVES ------------------------------------------------
# The HTTP wrapper, the parser-free JSON readers, find-by-name, the upsert,
# drift naming and the four verbs are scripts/lib/runner-scheduler.sh, shared
# with scripts/schedule-findings-steward.sh (plan 2026-09-17-findings-carry-a-
# triage-stamp-and-the-steward-reads-since-last-run, Phase 5). This file owns
# only what is return-to-main's: the task name, the prompt shape (--act /
# --shadow, --not-after), the turn and time budgets, and the description. A
# copy of this file with no lib/ beside it refuses (exit 2) rather than
# registering anything -- so lib/runner-scheduler.sh MUST travel with it: the
# skill's RTM_HELPERS roster already carries `lib`, and a Phase 6 move of this
# helper into the skill directory has to move the library too.
#
# -- USAGE -------------------------------------------------------------------
#   schedule-return-to-main.sh --install     upsert by name; the same options
#                                            twice -> the second says UNCHANGED
#   schedule-return-to-main.sh --check       present/absent, enabled, mode
#                                            (shadow/act), cron, next_run,
#                                            TZ_DRIFT, drift vs the intended body
#   schedule-return-to-main.sh --uninstall   delete every task named
#                                            return-to-main (exit 0 when none remain)
#   schedule-return-to-main.sh --dry-run     print the request bodies; no network
# Options (any verb; --uninstall ignores them):
#   --at HH:MM          daily fire time, local, 24 h (default 04:20)
#   --act               arm the job: the prompt carries --act instead of --shadow
#   --not-after HH:MM   handed to /return-to-main (default 06:30): a late
#                       catch-up fire after this local time reports and exits
#   --root DIR          workspace root (default: $QONTINUI_ROOT, else resolved
#                       from this script's own checkout)
#   --disabled          register the task DISABLED. The create route has no
#                       `enabled` field, so this is POST then PUT {"enabled":false};
#                       --check with --disabled expects it disabled.
#   -h | --help         this header
#
# Environment:
#   QONTINUI_RUNNER_URL  runner base URL (default http://127.0.0.1:9876). The
#                        scheduler service runs on the PRIMARY runner only, so
#                        this exists for fixtures, not for pointing at a
#                        secondary.
#   SCHEDULE_RTM_CURL    the curl binary (default `curl`). FIXTURE MODE: the
#                        hermetic test points it at a canned-response fake, so
#                        no live runner is needed to exercise every verb.
#
# Exit codes:
#   0  done; for --check, the task EXISTS (enabled or not -- the report says)
#   1  --check only: no task named return-to-main
#   2  UNKNOWN: the runner did not answer, answered non-2xx, or answered
#      something unreadable. Never reported as "absent".
#   3  the runner answered, but not with the asked-for state: more than one
#      task named return-to-main, or a write whose read-back does not match
#   4  usage
#
# Never writes an OS scheduler entry; never stops, restarts or rebuilds the
# runner; never touches a task with any other name.

set -u -o pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# An INHERITED GIT_DIR makes `git -C <path>` answer about a different
# repository, and git exports it into every hook's environment. Strip it once,
# as return-to-main-sweep.sh does. This script's only git call is a read-only
# workspace-root lookup, so a missing stripper is not a reason to refuse
# (absence of the library is not danger, #519): unset the variables directly.
if [ -r "$SCRIPT_DIR/lib/git-scope.sh" ]; then
    # shellcheck source=lib/git-scope.sh
    . "$SCRIPT_DIR/lib/git-scope.sh"
fi
if declare -F git_scope_strip >/dev/null 2>&1; then
    git_scope_strip
elif [ -n "${GIT_DIR+s}${GIT_WORK_TREE+s}${GIT_COMMON_DIR+s}" ]; then
    unset GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR GIT_INDEX_FILE
fi

# ---- the contract lib/runner-scheduler.sh reads -----------------------------
RS_SCRIPT="schedule-return-to-main"
TASK_NAME="return-to-main"
CURL="${SCHEDULE_RTM_CURL:-curl}"
FIX_HINT="/return-to-main --install (or: bash <path-to-this-skill-dir>/schedule-return-to-main.sh --install)"
# prompt_mode <unescaped prompt> -> act | shadow | shadow (implicit ...) | AMBIGUOUS
prompt_mode() {
    local p=" $1 " a=0 s=0
    case "$p" in *" --act "*) a=1 ;; esac
    case "$p" in *" --shadow "*) s=1 ;; esac
    if [ "$a" = 1 ] && [ "$s" = 1 ]; then printf 'AMBIGUOUS (the prompt carries both --act and --shadow)'
    elif [ "$a" = 1 ]; then printf 'act'
    elif [ "$s" = 1 ]; then printf 'shadow'
    else printf 'shadow (implicit: /return-to-main runs in shadow unless --act is passed)'
    fi
}

# The shared machinery is REQUIRED, not optional: without it there is no api(),
# no list_named and no verb, so a lone copy of this file can only refuse.
if [ ! -r "$SCRIPT_DIR/lib/runner-scheduler.sh" ]; then
    printf 'schedule-return-to-main: UNKNOWN -- lib/runner-scheduler.sh is not beside this helper at %s/lib/, so nothing could be read or written (a lone copy of the helper cannot register anything)\n' "$SCRIPT_DIR"
    exit 2
fi
# shellcheck source=lib/runner-scheduler.sh
. "$SCRIPT_DIR/lib/runner-scheduler.sh"

# ---- arguments --------------------------------------------------------------
VERB=""
AT="04:20"
NOT_AFTER="06:30"
ACT=0
DISABLED=0
ROOT_ARG=""
while [ $# -gt 0 ]; do
    case "$1" in
        --install|--check|--uninstall|--dry-run) set_verb "$1" ;;
        --at) shift; [ $# -gt 0 ] || usage_die "--at needs HH:MM"; AT="$1" ;;
        --at=*) AT="${1#*=}" ;;
        --not-after) shift; [ $# -gt 0 ] || usage_die "--not-after needs HH:MM"; NOT_AFTER="$1" ;;
        --not-after=*) NOT_AFTER="${1#*=}" ;;
        --act) ACT=1 ;;
        --disabled) DISABLED=1 ;;
        --root) shift; [ $# -gt 0 ] || usage_die "--root needs a directory"; ROOT_ARG="$1" ;;
        --root=*) ROOT_ARG="${1#*=}" ;;
        -h|--help) usage; exit 0 ;;
        *) usage_die "unknown argument $1" ;;
    esac
    shift
done
[ -n "$VERB" ] || usage_die "no verb: pass --install, --check, --uninstall or --dry-run"

valid_hhmm "$AT" || usage_die "--at must be HH:MM (24 h, local), got '$AT'"
valid_hhmm "$NOT_AFTER" || usage_die "--not-after must be HH:MM (24 h, local), got '$NOT_AFTER'"

ROOT=""
WORKDIR=""
if [ "$VERB" != uninstall ]; then
    rs_resolve_workdir
fi

# ---- the intended body ------------------------------------------------------
if [ "$ACT" = 1 ]; then MODE_FLAG=--act; else MODE_FLAG=--shadow; fi
PROMPT="/return-to-main $MODE_FLAG --not-after $NOT_AFTER"
MAX_TURNS=200
TIMEOUT_SECONDS=3600
DESC="Nightly /return-to-main for this device (plan 2026-09-13-nightly-return-to-main-sweep). Managed by qontinui-claude-config/scripts/schedule-return-to-main.sh; --check reports an edit made here as drift and the next --install reverts it."
rs_build_bodies

if [ "$VERB" = install ] && { [ "$NOT_AFTER" \< "$AT" ] || [ "$NOT_AFTER" = "$AT" ]; }; then
    printf 'schedule-return-to-main: warning: --not-after %s is not later than --at %s on the same day; a fire at %s may report and exit at once\n' "$NOT_AFTER" "$AT" "$AT" >&2
fi

rs_dispatch "$VERB"
