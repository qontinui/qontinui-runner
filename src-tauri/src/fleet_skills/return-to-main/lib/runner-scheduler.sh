#!/usr/bin/env bash
# Shared helper - SOURCE this, do not execute it.
#
# ascii-only-source
#
# The runner-scheduler registration machinery behind every "register ONE
# RemoteAgent task by name in the RUNNER'S OWN SCHEDULER" helper:
# scripts/schedule-return-to-main.sh (plan 2026-09-13-nightly-return-to-main-
# sweep, Phase 5c) and scripts/schedule-findings-steward.sh (plan
# 2026-09-17-findings-carry-a-triage-stamp-and-the-steward-reads-since-last-run,
# Phase 5). Extracted from the first so the second did not become a 250-line
# copy that drifts.
#
# -- THE ONLY SCHEDULING MECHANISM ---------------------------------------------
# Operator ruling 2026-09-13: scheduling happens INSIDE Qontinui -- in the
# runner, or spawned by coord -- never as a Windows Task Scheduler job, a
# systemd timer or a cron entry. Users run Windows, macOS and Linux, and nobody
# should need an external system to use Qontinui. Every caller of this library
# registers NOTHING with the operating system: it upserts a task in the
# runner's scheduler (persisted in the runner's Postgres, evaluated by its 60 s
# tick, a missed slot caught up once after a runner start) through the
# runner's HTTP API at 127.0.0.1:9876. bash, curl and a running primary runner
# are the whole dependency list, identically on every OS.
#
# -- WHAT A CALLER REGISTERS -------------------------------------------------
# API: runner src-tauri/src/mcp/scheduler.rs (routes /scheduler/tasks[/{id}]);
# shapes: qontinui-schemas rust/src/scheduler.rs (ScheduleExpression,
# ScheduledTaskType::RemoteAgent, CatchUpPolicy).
#   name              $TASK_NAME -- the UPSERT KEY. The runner has no unique
#                     name, so list_named finds the task by name through
#                     GET /scheduler/tasks and refuses (exit 3) when two exist.
#   schedule          {"type":"Cron","value":"<MM> <HH> * * *"} from --at. The
#                     runner prepends the seconds field of a 5-field cron
#                     itself (scheduler.rs compute_next_run).
#   task              {"task_type":"RemoteAgent", prompt, working_directory,
#                     max_turns, timeout_seconds}. The runner's own defaults
#                     (50 turns, 600 s) apply only when these are unset.
#   working_directory <workspace-root>, in the native (`cygpath -w`, backslash)
#                     spelling on Windows, because the runner hands it to a
#                     native process.
#   catchUpPolicy     run_once, stated explicitly although it is the default: a
#                     runner that was down at the slot fires ONE late run at
#                     start.
#   skipIfCompleted   false and autoFixOnFailure false, stated explicitly -- a
#                     skip-after-first-success task would run exactly once.
#
# -- CONTRACT ----------------------------------------------------------------
# The caller sets these BEFORE sourcing, then parses its own arguments:
#   RS_SCRIPT         its own short name, the prefix of every line it prints
#                     (`schedule-return-to-main`)
#   TASK_NAME         the upsert key
#   CURL              the curl binary; each caller exposes its own fixture
#                     variable for it (SCHEDULE_RTM_CURL, SCHEDULE_FS_CURL)
#   FIX_HINT          what --check names when the task is ABSENT
#   SCRIPT_DIR        the caller's own directory (root resolution starts there)
#   prompt_mode()     a function mapping an UNESCAPED prompt to the mode word
#                     the reports print (`act` / `shadow`, `write` / `report`)
# After parsing it sets AT (HH:MM) -- or RS_CRON, a whole 5-field cron that
# overrides AT for a cadence that is not once a day --, ROOT_ARG, DISABLED, PROMPT, MAX_TURNS,
# TIMEOUT_SECONDS and DESC, then calls:
#   rs_resolve_workdir  -> ROOT, WORKDIR (skipped for --uninstall)
#   rs_build_bodies     -> CRON, WANT_ENABLED, CREATE_BODY, UPDATE_BODY
#   rs_dispatch <verb>  -> do_dry_run | do_install | do_check | do_uninstall
# Everything else here is the shared plumbing those verbs run on.
#
# -- TIME ZONE: WHAT TZ_DRIFT MEANS -------------------------------------------
# A runner build predating plan 2026-09-13-nightly-return-to-main-sweep Phase
# 5a evaluates cron in UTC (compute_next_run and the missed-run enumerator both
# work in Utc; the scheduler's `timezone` setting is stored and never read).
# Measured 2026-09-13 on a UTC+2 box: cron `20 4 * * *` -> nextRun 04:20Z =
# 06:20 local, two hours late. --check converts the task's nextRun into THIS
# machine's local time (as `date` sees it) and compares it with the hour and
# minute in the task's own cron; a mismatch prints TZ_DRIFT. The library
# deliberately does NOT re-express the cron in UTC to compensate: that would
# fire at the wrong time the day 5a lands, and would be off by an hour for
# half of every year across DST.
#
# -- EXIT CODES (every caller documents the same table) -----------------------
#   0  done; for --check, the task EXISTS (enabled or not -- the report says)
#   1  --check only: no task named $TASK_NAME
#   2  UNKNOWN: the runner did not answer, answered non-2xx, or answered
#      something unreadable. Never reported as "absent".
#   3  the runner answered, but not with the asked-for state: more than one
#      task named $TASK_NAME, or a write whose read-back does not match
#   4  usage
#
# Never writes an OS scheduler entry; never stops, restarts or rebuilds the
# runner; never touches a task with any other name.

# resolve_root below runs `git -C`, and an inherited GIT_DIR overrides -C, so
# this file wires lib/git-scope.sh itself (lint-git-scope.py is per file) from
# its own directory -- the library IS in lib/, so the stripper is a sibling.
# Every caller has already stripped before sourcing; git_scope_strip is
# one-way and idempotent, so a second call costs nothing. Deliberately NO
# `unset` fallback here: that arm belongs to the caller (it is the arm
# schedule-return-to-main-test.sh's M6/M7 mutate), and a copy of this library
# staged without git-scope.sh beside it must not strip on the caller's behalf.
_rs_lib_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ -r "$_rs_lib_dir/git-scope.sh" ]; then
    # shellcheck source=git-scope.sh
    . "$_rs_lib_dir/git-scope.sh"   # lib/git-scope.sh, the bash stripper
fi
if declare -F git_scope_strip >/dev/null 2>&1; then
    git_scope_strip
fi

BASE="${QONTINUI_RUNNER_URL:-http://127.0.0.1:9876}"
BASE="${BASE%/}"
# The runner's /health has been sampled at 10 s on a loaded box; a scheduler
# read also goes through its Postgres. Sized against that tail.
TIMEOUT=20

say() { printf '%s: %s\n' "$RS_SCRIPT" "$*"; }
usage() { sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'; }
usage_die() { printf '%s: %s (try --help)\n' "$RS_SCRIPT" "$*" >&2; exit 4; }
die_unknown() { say "UNKNOWN -- $*"; exit 2; }
die_conflict() { say "$*"; exit 3; }
snip() { printf '%s' "$1" | tr '\n\r' '  ' | cut -c1-240; }

valid_hhmm() { [[ $1 =~ ^([01][0-9]|2[0-3]):[0-5][0-9]$ ]]; }
set_verb() {
    [ -z "$VERB" ] || usage_die "one verb at a time (got --$VERB and $1)"
    VERB="${1#--}"
}

# ---- workspace root ---------------------------------------------------------
# Same marker as return-to-main-sweep.sh's _rtm_is_root: the workspace root is a
# multi-repo umbrella that is not itself a git repo. From a LINKED WORKTREE the
# caller's own checkout sits under agent-worktrees/, so the root is derived
# from the git COMMON dir (the primary checkout), not from `..` of the script.
is_root() {
    case "$1" in ""|/|//) return 1 ;; esac
    [ -e "$1/qontinui-claude-config/.git" ] && [ -f "$1/.claude/settings.json" ]
}
resolve_root() {
    local common main d
    if [ -n "${ROOT_ARG:-}" ]; then
        # Explicit --root is honoured without the marker probe (fixtures, and a
        # non-standard layout); a path that is not a directory is still refused.
        [ -d "$ROOT_ARG" ] || return 1
        (cd "$ROOT_ARG" && pwd); return
    fi
    if [ -n "${QONTINUI_ROOT:-}" ] && is_root "$QONTINUI_ROOT"; then
        (cd "$QONTINUI_ROOT" && pwd); return
    fi
    common="$(git -C "$SCRIPT_DIR" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || common=""
    if [ -n "$common" ]; then
        main="$(dirname "$common")"
        d="$(dirname "$main")"
        is_root "$d" && { (cd "$d" && pwd); return; }
    fi
    # No git (a bundled copy): walk up from the script.
    d="$SCRIPT_DIR"
    while [ -n "$d" ] && [ "$d" != / ]; do
        is_root "$d" && { (cd "$d" && pwd); return; }
        d="${d%/*}"
    done
    return 1
}
# Native spelling for the runner. Same capture-then-test shape as
# lib/native-path.sh native_path_w (a failed cygpath must not yield an empty or
# two-line value).
to_native() {
    local n
    if command -v cygpath >/dev/null 2>&1 && n="$(cygpath -w "$1" 2>/dev/null)" && [ -n "$n" ]; then
        printf '%s' "$n"
    else
        printf '%s' "$1"
    fi
}
rs_resolve_workdir() {
    ROOT="$(resolve_root)" || usage_die "cannot resolve the workspace root (pass --root DIR, or set \$QONTINUI_ROOT)"
    WORKDIR="$(to_native "$ROOT")"
    case "$WORKDIR" in *[[:cntrl:]]*) usage_die "the workspace root contains a control character: $WORKDIR" ;; esac
}

# ---- the intended body ------------------------------------------------------
jesc() { local s="$1"; s="${s//\\/\\\\}"; s="${s//\"/\\\"}"; printf '%s' "$s"; }
junesc() { local s="$1"; s="${s//\\\"/\"}"; s="${s//\\\\/\\}"; printf '%s' "$s"; }

rs_build_bodies() {
    # RS_CRON, when a caller sets it, is the whole 5-field cron and wins over
    # --at: it is how a caller registers a cadence that is not once a day
    # (scripts/schedule-fleet-bundle-sync.sh: every N hours). Unset, the cron
    # is the one fixed daily time --at names, exactly as before.
    if [ -n "${RS_CRON:-}" ]; then
        CRON="$RS_CRON"
    else
        CRON="$((10#${AT#*:})) $((10#${AT%%:*})) * * *"
    fi
    if [ "${DISABLED:-0}" = 1 ]; then WANT_ENABLED=false; else WANT_ENABLED=true; fi
    SCHEDULE_JSON="{\"type\":\"Cron\",\"value\":\"$CRON\"}"
    TASK_JSON="{\"task_type\":\"RemoteAgent\",\"prompt\":\"$(jesc "$PROMPT")\",\"working_directory\":\"$(jesc "${WORKDIR:-}")\",\"max_turns\":$MAX_TURNS,\"timeout_seconds\":$TIMEOUT_SECONDS}"
    COMMON_JSON="\"schedule\":$SCHEDULE_JSON,\"task\":$TASK_JSON,\"skipIfCompleted\":false,\"autoFixOnFailure\":false,\"catchUpPolicy\":\"run_once\""
    CREATE_BODY="{\"name\":\"$TASK_NAME\",\"description\":\"$(jesc "$DESC")\",$COMMON_JSON}"
    UPDATE_BODY="{\"name\":\"$TASK_NAME\",\"description\":\"$(jesc "$DESC")\",\"enabled\":$WANT_ENABLED,$COMMON_JSON}"
}

# ---- HTTP -------------------------------------------------------------------
# api METHOD PATH [BODY] -> API_CODE, API_BODY, API_RC. Returns 1 when no HTTP
# answer was obtained at all (API_CODE=000). The body goes over STDIN, never
# argv: curl here is a native binary under MSYS, and argv path conversion would
# rewrite a JSON value that looks like a path.
API_CODE=""; API_BODY=""; API_RC=0
api() {
    local method="$1" path="$2" out
    API_RC=0
    if [ $# -ge 3 ]; then
        out="$(printf '%s' "$3" | "$CURL" -sS --max-time "$TIMEOUT" -X "$method" \
            -H 'Content-Type: application/json' --data-binary @- \
            -w '\n%{http_code}' "$BASE$path" 2>/dev/null)" || API_RC=$?
    else
        out="$("$CURL" -sS --max-time "$TIMEOUT" -X "$method" \
            -w '\n%{http_code}' "$BASE$path" 2>/dev/null)" || API_RC=$?
    fi
    API_CODE="${out##*$'\n'}"
    if [ "$out" = "$API_CODE" ]; then API_BODY=""; else API_BODY="${out%$'\n'*}"; fi
    if [ "$API_RC" -ne 0 ] || ! [[ $API_CODE =~ ^[0-9]{3}$ ]] || [ "$API_CODE" = 000 ]; then
        API_CODE=000
        return 1
    fi
    return 0
}

# ---- reading the runner's JSON without a JSON parser ------------------------
# bash + curl only, so the helpers run where neither jq nor python does. Two
# properties of serde_json's compact output make this exact rather than
# heuristic:
#   * `{"` cannot occur inside a string (every `"` in a string is escaped), so
#     splitting the list on `{"id":"` yields one line per TOP-LEVEL task --
#     ScheduledTask serializes `id` first and no nested object starts with it.
#   * `[[ =~ ]]` returns the LEFTMOST match, and a task's own `enabled`,
#     `description` and `name` precede every nested object that could carry the
#     same key (conditions.requireIdle.enabled comes later).
RE_STR='"(([^"\\]|\\.)*)"'
RE_HEAD='^\{"id":"([^"]*)","name":"(([^"\\]|\\.)*)"'
RE_SCHED='"schedule":\{"type":"([^"]*)","value":"(([^"\\]|\\.)*)"\}'
RE_CREATED='"data":\{"id":"([^"]*)"'
jstr() { # <json> <key> -> the raw (still escaped) value of the FIRST "key":"..."
    local re="\"$2\":$RE_STR"
    [[ $1 =~ $re ]] || return 1
    printf '%s' "${BASH_REMATCH[1]}"
}
jscalar() { # <json> <key> -> the FIRST "key":<number|true|false|null>
    local re="\"$2\":(-?[0-9]+|true|false|null)"
    [[ $1 =~ $re ]] || return 1
    printf '%s' "${BASH_REMATCH[1]}"
}
split_tasks() {
    local marker='{"id":"' b="$1"
    b="${b//"$marker"/$'\n'"$marker"}"
    printf '%s\n' "$b" | grep '^{"id":"'
}
task_id() { [[ $1 =~ $RE_HEAD ]] && printf '%s' "${BASH_REMATCH[1]}"; }

# list_named -> TASKS (every task named $TASK_NAME), ALL_COUNT, LIST_EMPTY
TASKS=(); ALL_COUNT=0; LIST_EMPTY=0
list_named() {
    local line
    api GET /scheduler/tasks || die_unknown "the runner at $BASE did not answer GET /scheduler/tasks (curl exit $API_RC) -- whether a $TASK_NAME task exists is UNKNOWN, not absent"
    [ "$API_CODE" = 200 ] || die_unknown "GET $BASE/scheduler/tasks answered HTTP $API_CODE: $(snip "$API_BODY")"
    case "$API_BODY" in
        *'"success":true'*'"data":['*) ;;
        *) die_unknown "GET $BASE/scheduler/tasks answered 200 without the runner's {\"success\":true,\"data\":[...]} envelope: $(snip "$API_BODY")" ;;
    esac
    LIST_EMPTY=0
    case "$API_BODY" in *'"data":[]'*) LIST_EMPTY=1 ;; esac
    TASKS=(); ALL_COUNT=0
    while IFS= read -r line; do
        [[ $line =~ $RE_HEAD ]] || continue
        ALL_COUNT=$((ALL_COUNT + 1))
        [ "${BASH_REMATCH[2]}" = "$TASK_NAME" ] && TASKS+=("$line")
    done < <(split_tasks "$API_BODY")
}

# ---- drift: a found task against the intended body --------------------------
DRIFT=""
drift_add() { DRIFT="${DRIFT:+$DRIFT; }$1"; }
compute_drift() { # <task json>
    local c="$1" v want
    DRIFT=""
    v="$(jscalar "$c" enabled)" || v="<absent>"
    [ "$v" = "$WANT_ENABLED" ] || drift_add "enabled is $v, intended $WANT_ENABLED"
    v="$(jstr "$c" description)" || v="<absent>"
    [ "$v" = "$(jesc "$DESC")" ] || drift_add "description differs"
    if [[ $c =~ $RE_SCHED ]]; then
        [ "${BASH_REMATCH[1]}" = Cron ] && [ "${BASH_REMATCH[2]}" = "$CRON" ] \
            || drift_add "schedule is ${BASH_REMATCH[1]} '$(junesc "${BASH_REMATCH[2]}")', intended Cron '$CRON'"
    else
        drift_add "schedule is not a Cron string (intended Cron '$CRON')"
    fi
    v="$(jstr "$c" task_type)" || v="<absent>"
    [ "$v" = RemoteAgent ] || drift_add "task_type is $v, intended RemoteAgent"
    v="$(jstr "$c" prompt)" || v="<absent>"
    [ "$v" = "$(jesc "$PROMPT")" ] || drift_add "prompt is '$(junesc "$v")', intended '$PROMPT'"
    v="$(jstr "$c" working_directory)" || v="<absent>"
    [ "$v" = "$(jesc "$WORKDIR")" ] || drift_add "working_directory is '$(junesc "$v")', intended '$WORKDIR'"
    v="$(jscalar "$c" max_turns)" || v="<absent>"
    [ "$v" = "$MAX_TURNS" ] || drift_add "max_turns is $v, intended $MAX_TURNS"
    v="$(jscalar "$c" timeout_seconds)" || v="<absent>"
    [ "$v" = "$TIMEOUT_SECONDS" ] || drift_add "timeout_seconds is $v, intended $TIMEOUT_SECONDS"
    for want in model allowed_tools mcp_connections; do
        case "$c" in *"\"$want\":"*) drift_add "task carries $want, which the intended body leaves unset" ;; esac
    done
    v="$(jscalar "$c" skipIfCompleted)" || v="<absent>"
    [ "$v" = false ] || drift_add "skipIfCompleted is $v, intended false"
    v="$(jscalar "$c" autoFixOnFailure)" || v="<absent>"
    [ "$v" = false ] || drift_add "autoFixOnFailure is $v, intended false"
    v="$(jstr "$c" catchUpPolicy)" || v="<absent>"
    [ "$v" = run_once ] || drift_add "catchUpPolicy is $v, intended run_once"
}

# ---- reporting helpers ------------------------------------------------------
local_hhmm() { # <RFC 3339> -> HH:MM in this machine's local zone; empty when unconvertible
    local iso="$1" out
    out="$(date -d "$iso" +%H:%M 2>/dev/null)" && [ -n "$out" ] && { printf '%s' "$out"; return 0; }
    # BSD date (macOS): no -d; parse the fixed RFC 3339 shape the runner writes.
    if [[ $iso =~ ^([0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2})(\.[0-9]+)?(Z|[+-][0-9]{2}:[0-9]{2})$ ]]; then
        local base="${BASH_REMATCH[1]}" off="${BASH_REMATCH[3]}"
        [ "$off" = Z ] && off="+00:00"
        out="$(date -j -f '%Y-%m-%dT%H:%M:%S%z' "$base${off/:/}" +%H:%M 2>/dev/null)" && [ -n "$out" ] && printf '%s' "$out"
    fi
}
cron_hhmm() { # <cron> -> HH:MM when it is one fixed daily time, else empty
    local f
    read -r -a f <<<"$1"
    [ "${#f[@]}" -eq 6 ] && f=("${f[@]:1}")
    [ "${#f[@]}" -eq 5 ] || return 0
    [[ ${f[0]} =~ ^[0-9]+$ && ${f[1]} =~ ^[0-9]+$ ]] || return 0
    [ "${f[2]}" = '*' ] && [ "${f[3]}" = '*' ] && [ "${f[4]}" = '*' ] || return 0
    printf '%02d:%02d' "$((10#${f[1]}))" "$((10#${f[0]}))"
}
# report_task <task json>: the --check body for one task.
report_task() {
    local c="$1" id en prompt cron wd mt ts cp next lhm chm tz
    id="$(task_id "$c")" || id="$(jstr "$c" id)" || id="?"
    en="$(jscalar "$c" enabled)" || en="UNREADABLE"
    prompt="$(jstr "$c" prompt)" && prompt="$(junesc "$prompt")" || prompt="<absent>"
    cron=""
    [[ $c =~ $RE_SCHED ]] && cron="$(junesc "${BASH_REMATCH[2]}")"
    wd="$(jstr "$c" working_directory)" && wd="$(junesc "$wd")" || wd="<absent>"
    mt="$(jscalar "$c" max_turns)" || mt="<unset: runner default>"
    ts="$(jscalar "$c" timeout_seconds)" || ts="<unset: runner default>"
    cp="$(jstr "$c" catchUpPolicy)" || cp="<absent>"
    next="$(jstr "$c" nextRun)" || next=""
    printf '  id:                %s\n' "$id"
    printf '  enabled:           %s\n' "$en"
    printf '  mode:              %s\n' "$(prompt_mode "$prompt")"
    printf '  prompt:            %s\n' "$prompt"
    printf '  cron:              %s\n' "${cron:-<not a Cron schedule>}"
    printf '  working_directory: %s\n' "$wd"
    printf '  max_turns:         %s   timeout_seconds: %s   catch_up_policy: %s\n' "$mt" "$ts" "$cp"
    chm="$(cron_hhmm "$cron")"
    if [ -z "$next" ]; then
        printf '  next_run:          <none reported>%s\n' "$([ "$en" = false ] && printf ' (a disabled task carries no nextRun)')"
        tz="not checked -- no nextRun to compare"
    else
        lhm="$(local_hhmm "$next")"
        printf '  next_run:          %s%s\n' "$next" "${lhm:+ = $lhm local}"
        if [ -z "$chm" ]; then
            tz="not checked -- the cron is not one fixed daily time"
        elif [ -z "$lhm" ]; then
            tz="UNKNOWN -- could not convert nextRun to local time on this machine"
        elif [ "$lhm" = "$chm" ]; then  # tz-compare
            tz="MATCH -- the runner fires at $lhm local, as the cron says"
        else
            tz="TZ_DRIFT -- the runner's next fire is $lhm local but the cron says $chm; this runner build evaluates cron in UTC (plan Phase 5a), so the job runs at the wrong local time"
        fi
    fi
    printf '  tz_check:          %s\n' "$tz"
}

# ---- verbs ------------------------------------------------------------------
do_dry_run() {
    say "DRY-RUN -- nothing was sent to the runner and nothing was touched"
    printf '  runner:            %s\n' "$BASE"
    printf '  workspace root:    %s (working_directory %s)\n' "$ROOT" "$WORKDIR"
    printf '  mode:              %s\n' "$(prompt_mode "$PROMPT")"
    printf '  create (no task named %s yet): POST /scheduler/tasks\n' "$TASK_NAME"
    printf '%s\n' "$CREATE_BODY"
    printf '  update (one exists): PUT /scheduler/tasks/<id>\n'
    printf '%s\n' "$UPDATE_BODY"
    [ "${DISABLED:-0}" = 1 ] && printf '  then, after a create: PUT /scheduler/tasks/<id> {"enabled":false}\n'
    exit 0
}

refuse_duplicates() {
    local c ids=""
    for c in "${TASKS[@]}"; do ids="${ids:+$ids, }$(task_id "$c") (enabled=$(jscalar "$c" enabled || printf '?'))"; done
    die_conflict "DUPLICATE -- ${#TASKS[@]} tasks are named $TASK_NAME at $BASE: $ids. An upsert by name cannot choose between them; remove the extras in the runner's Scheduler UI, or run --uninstall (it removes all of them) and then --install."
}

# verify_by_id <id>: read the task back by id and require the intended body.
verify_by_id() {
    api GET "/scheduler/tasks/$1" || die_unknown "wrote task $1, then GET /scheduler/tasks/$1 got no answer (curl exit $API_RC) -- the write is UNVERIFIED"
    [ "$API_CODE" = 200 ] || die_unknown "wrote task $1, then GET /scheduler/tasks/$1 answered HTTP $API_CODE -- the write is UNVERIFIED: $(snip "$API_BODY")"
    compute_drift "$API_BODY"
    [ -z "$DRIFT" ] || die_conflict "MISMATCH -- task $1 was written but reads back differently: $DRIFT"
}

do_install() {
    local n id c
    list_named
    n=${#TASKS[@]}
    if [ "$n" -gt 1 ]; then refuse_duplicates; fi  # refuse-duplicates
    if [ "$n" -eq 0 ]; then
        api POST /scheduler/tasks "$CREATE_BODY" || die_unknown "POST $BASE/scheduler/tasks got no answer (curl exit $API_RC) -- whether the task was created is UNKNOWN; run --check"
        case "$API_CODE" in 200|201) ;; *) die_unknown "POST $BASE/scheduler/tasks answered HTTP $API_CODE: $(snip "$API_BODY")" ;; esac
        [[ $API_BODY =~ $RE_CREATED ]] || die_unknown "POST answered $API_CODE but no task id could be read from it: $(snip "$API_BODY")"
        id="${BASH_REMATCH[1]}"
        if [ "${DISABLED:-0}" = 1 ]; then
            api PUT "/scheduler/tasks/$id" '{"enabled":false}' || die_unknown "created task $id, then PUT {\"enabled\":false} got no answer -- it may be ENABLED; run --check"
            case "$API_CODE" in 200|201|204) ;; *) die_unknown "created task $id, then PUT {\"enabled\":false} answered HTTP $API_CODE -- it may be ENABLED: $(snip "$API_BODY")" ;; esac
        fi
        verify_by_id "$id"
        # The upsert key is the NAME, found through the list route -- and that
        # route answers an empty list when its own Postgres read fails
        # (mcp/scheduler.rs list_scheduled_tasks). If the new task is not
        # listed, every later --install would create another one.
        list_named
        [ "${#TASKS[@]}" -eq 1 ] && [ "$(task_id "${TASKS[0]}")" = "$id" ] \
            || die_conflict "MISMATCH -- created task $id (it reads back by id), but GET /scheduler/tasks lists ${#TASKS[@]} task(s) named $TASK_NAME; the next --install could not find it by name. Not retrying."
        say "CREATED id=$id enabled=$WANT_ENABLED mode=$(prompt_mode "$PROMPT") cron='$CRON' at $BASE"
        exit 0
    fi
    c="${TASKS[0]}"
    id="$(task_id "$c")"
    compute_drift "$c"
    if [ -z "$DRIFT" ]; then  # unchanged
        say "UNCHANGED id=$id enabled=$WANT_ENABLED mode=$(prompt_mode "$PROMPT") cron='$CRON' at $BASE"
        exit 0
    fi
    local was="$DRIFT"
    api PUT "/scheduler/tasks/$id" "$UPDATE_BODY" || die_unknown "PUT $BASE/scheduler/tasks/$id got no answer (curl exit $API_RC) -- whether it was updated is UNKNOWN; run --check"
    case "$API_CODE" in 200|201|204) ;; *) die_unknown "PUT $BASE/scheduler/tasks/$id answered HTTP $API_CODE: $(snip "$API_BODY")" ;; esac
    verify_by_id "$id"
    say "UPDATED id=$id enabled=$WANT_ENABLED mode=$(prompt_mode "$PROMPT") cron='$CRON' at $BASE (was: $was)"
    exit 0
}

do_check() {
    local n
    list_named
    n=${#TASKS[@]}
    if [ "$n" -eq 0 ]; then
        say "ABSENT -- no runner scheduler task named $TASK_NAME at $BASE ($ALL_COUNT task(s) with other names)"
        [ "$LIST_EMPTY" = 1 ] && printf '  note: the list was EMPTY, and the runner answers an empty list when its own database read fails too (mcp/scheduler.rs list_scheduled_tasks), so this is as good as that read\n'
        printf '  fix: %s\n' "$FIX_HINT"
        exit 1
    fi
    if [ "$n" -gt 1 ]; then refuse_duplicates; fi  # refuse-duplicates
    say "PRESENT -- runner scheduler task $TASK_NAME at $BASE"
    report_task "${TASKS[0]}"
    compute_drift "${TASKS[0]}"
    if [ -z "$DRIFT" ]; then
        printf '  drift:             none (matches the intended body for these options)\n'
    else
        printf '  drift:             %s -- --install with the same options would rewrite it\n' "$DRIFT"
    fi
    exit 0
}

do_uninstall() {
    local c id n removed=""
    list_named
    n=${#TASKS[@]}
    if [ "$n" -eq 0 ]; then
        say "ABSENT -- no task named $TASK_NAME at $BASE; nothing to remove"
        exit 0
    fi
    for c in "${TASKS[@]}"; do
        id="$(task_id "$c")"
        api DELETE "/scheduler/tasks/$id" || die_unknown "DELETE $BASE/scheduler/tasks/$id got no answer (curl exit $API_RC) -- whether it was removed is UNKNOWN; run --check"
        case "$API_CODE" in 200|204) ;; *) die_unknown "DELETE $BASE/scheduler/tasks/$id answered HTTP $API_CODE: $(snip "$API_BODY")" ;; esac
        removed="${removed:+$removed, }$id"
    done
    list_named
    [ "${#TASKS[@]}" -eq 0 ] || die_conflict "MISMATCH -- deleted $removed, but GET /scheduler/tasks still lists ${#TASKS[@]} task(s) named $TASK_NAME"
    say "REMOVED $n task(s) named $TASK_NAME at $BASE: $removed"
    exit 0
}

rs_dispatch() {
    case "$1" in
        dry-run)   do_dry_run ;;
        install)   do_install ;;
        check)     do_check ;;
        uninstall) do_uninstall ;;
        *) usage_die "no verb: pass --install, --check, --uninstall or --dry-run" ;;
    esac
}
