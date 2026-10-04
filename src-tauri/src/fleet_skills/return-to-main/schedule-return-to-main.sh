#!/usr/bin/env bash
# schedule-return-to-main.sh - register (or check / remove) the nightly
# /return-to-main session as ONE task, named `return-to-main`, in the RUNNER'S
# OWN SCHEDULER.
#
# Plan: 2026-09-13-nightly-return-to-main-sweep (Phase 5c); opportunistic
# scheduling by plan 2026-09-29-quiet-is-measured-by-session-existence-and-
# machine-wide-so-a-24x7-box-never-gets-one (Phase 5).
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
# -- TWO MODES: A PROBE-GATED CONDITION, OR THE INTERIM CRON -------------------
# A box that is never empty of sessions has no quiet hour to aim a clock at
# (measured 2026-09-29: no local hour ever idle, the old 04:20 slot busier than
# the 07:00-09:00 trough). So where the runner can run a probe, the task is not
# a clock at all:
#   condition  schedule {"type":"Condition","value":{"rearmDelayMinutes":120}}
#              with conditions {"requireProbe":{"enabled":true,"command":[
#              <bash>, <machine-quiesce-check.sh>, --for, checkout-ff,
#              --exit-quiet-if-any-repo, --root, <root>],"pollSeconds":300,
#              "timeoutSeconds":120}} -- the runner runs the quiet check every
#              5 minutes and fires the job whenever it exits 0 (some repo is
#              QUIET for a fast-forward AND has something to do), then waits
#              2 h before looking again. The prompt carries --daily-cap N
#              (default 3) instead of --not-after: a job that can fire at any
#              hour is bounded by a count, which the skill enforces (Step 1a).
#   cron       the interim clock: 07:20 local (the measured trough), prompt
#              --not-after 09:30. Registered whenever the condition form is
#              not PROVEN safe.
# WHICH ONE IS A CAPABILITY READ, NEVER A VERSION GUESS. The runner's serde
# silently DROPS an unknown `requireProbe`, so a Condition schedule posted to a
# build that does not enforce it loses its only gate and fires every 2 h around
# the clock. --install / --check / --dry-run therefore read GET <runner>/health
# first and take the condition form ONLY when its `schedulerConditions` array
# (top level or under `data`) contains "require_probe" -- which the runner
# advertises only when it both evaluates the probe and persists a task's
# conditions. No answer, a non-2xx, no such field, a field that is not an
# array, or an array without the entry is UNKNOWN, and UNKNOWN installs the
# cron and says why. --mode cron forces the cron (there is no forcing the
# condition form). --check repeats the read: a Condition task on a runner that
# no longer advertises require_probe is exit 3 (UNGATED), and one on a runner
# whose capability could not be read is exit 2.
#
# -- WHAT IT REGISTERS --------------------------------------------------------
# API: runner src-tauri/src/mcp/scheduler.rs (routes /scheduler/tasks[/{id}]);
# shapes: qontinui-schemas rust/src/scheduler.rs (ScheduleExpression,
# ScheduledTaskType::RemoteAgent, CatchUpPolicy).
#   name              return-to-main   -- the UPSERT KEY. The runner has no
#                     unique name, so the helper finds the task by name through
#                     GET /scheduler/tasks and refuses (exit 3) when two exist.
#   schedule          cron mode: {"type":"Cron","value":"<MM> <HH> * * *"} from
#                     --at (default 07:20). The runner prepends the seconds
#                     field of a 5-field cron itself (scheduler.rs
#                     compute_next_run). Condition mode: see above.
#   conditions        condition mode: the requireProbe above. Cron mode: `{}`,
#                     the one spelling that clears a probe a previous
#                     condition-mode install left behind.
#   task              {"task_type":"RemoteAgent", prompt, working_directory,
#                     max_turns 200, timeout_seconds 3600}. The runner's own
#                     defaults (50 turns, 600 s) apply only when these are unset,
#                     and a sweep plus adjudication does not fit in them.
#   prompt            cron:      /return-to-main --shadow --not-after 09:30
#                                (--act: /return-to-main --act --not-after 09:30)
#                     condition: /return-to-main --shadow --daily-cap 3
#                                (--act: /return-to-main --act --daily-cap 3)
#                     The skill runs in SHADOW unless --act is passed, so arming
#                     the job means the prompt CONTAINS --act; omitting --shadow
#                     alone would leave it in shadow.
#   working_directory <workspace-root>, in the native (`cygpath -w`, backslash)
#                     spelling on Windows, because the runner hands it to a
#                     native process.
#   catchUpPolicy     run_once, stated explicitly although it is the default: a
#                     runner that was down at 07:20 fires ONE late run at start,
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
#   schedule-return-to-main.sh --dry-run     print the request bodies; the only
#                                            request is the read-only GET /health
#                                            that picks the mode
# Options (any verb; --uninstall ignores them):
#   --at HH:MM          cron mode: daily fire time, local, 24 h (default 07:20)
#   --act               arm the job: the prompt carries --act instead of --shadow
#   --not-after HH:MM   cron mode: handed to /return-to-main (default 09:30): a
#                       late catch-up fire after this local time reports and exits
#   --daily-cap N       condition mode: handed to /return-to-main (default 3,
#                       1..24): the most runs the skill starts in one local day
#   --mode auto|cron    auto (default): condition when the runner advertises
#                       require_probe, else cron. cron: always the interim cron.
#   --probe-script PATH condition mode: the machine-quiesce-check.sh the probe
#                       runs (default: the one this helper was copied from, per
#                       the run's RESOLVED list, else the one beside it)
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
AT="07:20"
NOT_AFTER="09:30"
DAILY_CAP=3
MODE_ARG=auto
PROBE_SCRIPT_ARG=""
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
        --daily-cap) shift; [ $# -gt 0 ] || usage_die "--daily-cap needs a number"; DAILY_CAP="$1" ;;
        --daily-cap=*) DAILY_CAP="${1#*=}" ;;
        --mode) shift; [ $# -gt 0 ] || usage_die "--mode needs auto or cron"; MODE_ARG="$1" ;;
        --mode=*) MODE_ARG="${1#*=}" ;;
        --probe-script) shift; [ $# -gt 0 ] || usage_die "--probe-script needs a path"; PROBE_SCRIPT_ARG="$1" ;;
        --probe-script=*) PROBE_SCRIPT_ARG="${1#*=}" ;;
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
[[ $DAILY_CAP =~ ^[0-9]+$ ]] && [ "$((10#$DAILY_CAP))" -ge 1 ] && [ "$((10#$DAILY_CAP))" -le 24 ] \
    || usage_die "--daily-cap must be a whole number 1..24, got '$DAILY_CAP'"
DAILY_CAP="$((10#$DAILY_CAP))"
case "$MODE_ARG" in auto|cron) ;; *) usage_die "--mode takes auto or cron (the condition form is never forced: it needs the runner's capability), got '$MODE_ARG'" ;; esac

ROOT=""
WORKDIR=""
if [ "$VERB" != uninstall ]; then
    rs_resolve_workdir
fi

# ---- which mode: the runner's own capability, read, never assumed -----------
# CAP_STATE: advertised | not_advertised | unknown; CAP_WHY says what was read.
CAP_STATE=unknown
CAP_WHY=""
read_capability() {
    local list
    if ! api GET /health; then
        CAP_WHY="the runner at $BASE did not answer GET /health (curl exit $API_RC)"; return
    fi
    if [ "$API_CODE" != 200 ]; then
        CAP_WHY="GET $BASE/health answered HTTP $API_CODE"; return
    fi
    case "$API_BODY" in
        *'"schedulerConditions":'*) ;;
        *) CAP_WHY="GET $BASE/health carries no schedulerConditions field (a runner build predating the capability, which would run a probe-gated task UNGATED)"; return ;;
    esac
    if [[ $API_BODY =~ \"schedulerConditions\":\[([^]]*)\] ]]; then
        list="${BASH_REMATCH[1]}"
    else
        CAP_WHY="GET $BASE/health: schedulerConditions is not an array"; return
    fi
    case ",$list," in
        *',"require_probe",'*) CAP_STATE=advertised; CAP_WHY="GET $BASE/health schedulerConditions [$list] includes require_probe" ;;
        *) CAP_STATE=not_advertised; CAP_WHY="GET $BASE/health schedulerConditions [$list] does not include require_probe" ;;
    esac
}

# The probe's quiet check: an explicit --probe-script; else, when this helper
# is a night's private copy (Step 0's $RUN_DIR/bin, marked by its RESOLVED
# list), the file that copy was taken from -- never the frozen copy itself;
# else the one beside this helper.
probe_script() {
    local p="" src
    if [ -n "$PROBE_SCRIPT_ARG" ]; then
        p="$PROBE_SCRIPT_ARG"
    elif [ -f "$SCRIPT_DIR/RESOLVED" ]; then
        src="$(awk -F '\t' '$1 == "machine-quiesce-check.sh" { print $2; exit }' "$SCRIPT_DIR/RESOLVED" 2>/dev/null)"
        [ -n "$src" ] && [ "$src" != MISSING ] && p="$src"
    fi
    [ -n "$p" ] || p="$SCRIPT_DIR/machine-quiesce-check.sh"
    [ -f "$p" ] || return 1
    (cd "$(dirname "$p")" && printf '%s/%s' "$(pwd)" "$(basename "$p")")
}

SCHED_MODE=cron
MODE_WHY=""
PROBE_ARGV=()
if [ "$VERB" != uninstall ]; then
    if [ "$MODE_ARG" = cron ]; then
        MODE_WHY="--mode cron"
    else
        read_capability
        if [ "$CAP_STATE" = advertised ]; then
            if PROBE_PATH="$(probe_script)"; then
                SCHED_MODE=condition
                MODE_WHY="$CAP_WHY"
                BASH_BIN="$(command -v bash 2>/dev/null)"; [ -n "$BASH_BIN" ] || BASH_BIN=bash
                PROBE_ARGV=("$(to_native "$BASH_BIN")" "$(to_native "$PROBE_PATH")" --for checkout-ff --exit-quiet-if-any-repo --root "$WORKDIR")
            else
                MODE_WHY="$CAP_WHY, but the quiet check the probe would run was not found (${PROBE_SCRIPT_ARG:-beside this helper}); a probe that cannot run is no gate"
            fi
        else
            MODE_WHY="$CAP_WHY -- UNKNOWN whether a probe gate would hold, so the interim cron"
        fi
    fi
fi

# ---- the intended body ------------------------------------------------------
if [ "$ACT" = 1 ]; then MODE_FLAG=--act; else MODE_FLAG=--shadow; fi
MAX_TURNS=200
TIMEOUT_SECONDS=3600
PROBE_POLL_SECONDS=300
PROBE_TIMEOUT_SECONDS=120
REARM_DELAY_MINUTES=120
if [ "$SCHED_MODE" = condition ]; then
    PROMPT="/return-to-main $MODE_FLAG --daily-cap $DAILY_CAP"
    RS_SCHEDULE_JSON="{\"type\":\"Condition\",\"value\":{\"rearmDelayMinutes\":$REARM_DELAY_MINUTES}}"
    _cmd=""
    for _a in "${PROBE_ARGV[@]}"; do _cmd="${_cmd:+$_cmd,}\"$(jesc "$_a")\""; done
    RS_CONDITIONS_JSON="{\"requireProbe\":{\"enabled\":true,\"command\":[$_cmd],\"pollSeconds\":$PROBE_POLL_SECONDS,\"timeoutSeconds\":$PROBE_TIMEOUT_SECONDS}}"
    DESC="Opportunistic /return-to-main for this device: fires when machine-quiesce-check.sh --for checkout-ff --exit-quiet-if-any-repo exits 0, at most $DAILY_CAP run(s) a day (plan 2026-09-29-quiet-is-measured-by-session-existence-and-machine-wide-so-a-24x7-box-never-gets-one). Managed by the return-to-main skill's schedule-return-to-main.sh; --check reports an edit made here as drift and the next --install reverts it."
else
    PROMPT="/return-to-main $MODE_FLAG --not-after $NOT_AFTER"
    RS_CONDITIONS_JSON="{}"
    DESC="Nightly /return-to-main for this device (plan 2026-09-13-nightly-return-to-main-sweep). Managed by the return-to-main skill's schedule-return-to-main.sh; --check reports an edit made here as drift and the next --install reverts it."
fi
rs_build_bodies

if [ "$VERB" = install ] && [ "$SCHED_MODE" = cron ] && { [ "$NOT_AFTER" \< "$AT" ] || [ "$NOT_AFTER" = "$AT" ]; }; then
    printf 'schedule-return-to-main: warning: --not-after %s is not later than --at %s on the same day; a fire at %s may report and exit at once\n' "$NOT_AFTER" "$AT" "$AT" >&2
fi

# The stored requireProbe, read FIELD BY FIELD: the runner serializes it from
# its own struct, and a key order this helper does not control must never read
# as drift (or hide one). Sets PROBE_BODY (the object's inside) or returns 1.
PROBE_BODY=""
stored_probe() {
    PROBE_BODY=""
    [[ $1 =~ \"requireProbe\":\{([^{}]*)\} ]] || return 1
    PROBE_BODY="${BASH_REMATCH[1]}"
}
probe_field() { # <key> -> the stored value (scalar, or the raw [ ... ] of an array)
    local re
    if [ "$1" = command ]; then re='(^|,)"command":\[([^]]*)\]'; else re="(^|,)\"$1\":([^,]*)"; fi
    [[ $PROBE_BODY =~ $re ]] || return 1
    printf '%s' "${BASH_REMATCH[2]}"
}
rs_conditions_drift() {
    local c="$1" want_cmd v k
    if ! stored_probe "$c"; then
        drift_add "the task carries no requireProbe (intended the quiet-check probe)"
        return 0
    fi
    want_cmd="${RS_CONDITIONS_JSON#*\"command\":[}"; want_cmd="${want_cmd%%]*}"
    v="$(probe_field enabled)" || v="<absent>"
    [ "$v" = true ] || drift_add "requireProbe.enabled is $v, intended true"
    v="$(probe_field command)" || v="<absent>"
    [ "$v" = "$want_cmd" ] || drift_add "requireProbe.command is [$v], intended [$want_cmd]"
    v="$(probe_field pollSeconds)" || v="<absent>"
    [ "$v" = "$PROBE_POLL_SECONDS" ] || drift_add "requireProbe.pollSeconds is $v, intended $PROBE_POLL_SECONDS"
    v="$(probe_field timeoutSeconds)" || v="<absent>"
    [ "$v" = "$PROBE_TIMEOUT_SECONDS" ] || drift_add "requireProbe.timeoutSeconds is $v, intended $PROBE_TIMEOUT_SECONDS"
    for k in requireIdle requireRepoInactive timeoutMinutes; do
        case "$c" in *"\"$k\":{"*|*"\"$k\":"[0-9]*) drift_add "the task carries conditions.$k, which the intended body leaves unset" ;; esac
    done
}

# --check: a probe-gated task is only as safe as the runner's enforcement of
# the probe -- AND only gated at all if the stored task carries an ENABLED
# requireProbe. Read against the capability read above, AFTER the report.
rs_check_extra() {
    case "$1" in
        *'"schedule":{"type":"Condition"'*) ;;
        *) return 0 ;;
    esac
    if ! stored_probe "$1" || [ "$(probe_field enabled)" != true ]; then
        say "UNGATED -- this return-to-main task is a Condition schedule with no ENABLED requireProbe: nothing gates it, so it fires every ${REARM_DELAY_MINUTES} min. --install puts the probe back."
        exit 3
    fi
    if [ "$CAP_STATE" = advertised ]; then
        printf '  gate:              the runner advertises require_probe, and the task carries an enabled requireProbe; this Condition task is gated\n'
        return 0
    fi
    if [ "$MODE_ARG" = cron ]; then
        printf '  gate:              not read (--mode cron); run --check without it to verify this Condition task is gated\n'
        return 0
    fi
    if [ "$CAP_STATE" = not_advertised ]; then
        say "UNGATED -- this return-to-main task is a Condition schedule, but $CAP_WHY: it fires every ${REARM_DELAY_MINUTES} min whether or not the machine is quiet. --install replaces it with the interim cron."
        exit 3
    fi
    say "UNKNOWN -- this return-to-main task is a Condition schedule, and whether the runner enforces its probe could not be read: $CAP_WHY"
    exit 2
}

if [ "$VERB" != uninstall ]; then
    say "scheduling mode: $SCHED_MODE ($MODE_WHY)"
fi
rs_dispatch "$VERB"
