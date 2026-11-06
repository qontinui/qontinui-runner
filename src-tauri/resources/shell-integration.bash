#!/usr/bin/env bash
# Qontinui Shell Integration for Bash (OSC 633 / VS Code compatible)
# Used as --rcfile so the shell sources it instead of ~/.bashrc.

# Guard: only install once
if [ "${QONTINUI_INTEGRATION-}" = "1" ]; then
    [ -f ~/.bashrc ] && source ~/.bashrc
    return 0 2>/dev/null || exit 0
fi
export QONTINUI_INTEGRATION=1

# The pane, by path: the terminal on stdin when this shell started, read
# BEFORE the user's rc can redirect anything (one fork; `command`, so no
# function or alias named `tty` stands in; silent if the fork itself fails).
# Empty when `tty` fails or prints no path, and then no prompt announces the
# capability (see "Prompt-state marks" below).
if { __qontinui_pane_tty="$(command tty 2>/dev/null)"; } 2>/dev/null; then
    case "${__qontinui_pane_tty}" in
        /*) ;;
        *) __qontinui_pane_tty="" ;;
    esac
else
    __qontinui_pane_tty=""
fi

# Source user's bashrc (skipped when used as --rcfile)
[ -f ~/.bashrc ] && source ~/.bashrc

# OSC 633 helper — writes directly to /dev/tty to avoid polluting stdout
__osc633() {
    printf '\033]633;%s\007' "$1" > /dev/tty
}

# ── Prompt-state marks ──────────────────────────────────────────────────────
# The rule: the user's shell must behave as without this block, apart from the
# OSC 633 bytes. Every PROMPT_COMMAND entry of theirs still runs, in order,
# seeing `$?`; a PS0 of theirs is kept (ours is prefixed); `set -e` / `set -u`
# in their rc survive (both entries run where errexit is suspended, every
# expansion has a default); a readonly PROMPT_COMMAND / PS0 / PS1 and a DEBUG
# trap of theirs are left alone. shell-integration-marks-test.sh checks the
# cases it names on a real pty; anything else is held to the same rule.
#
# Per prompt, from PROMPT_COMMAND (so a PROMPT_COMMAND that rewrites PS1 —
# starship, __git_ps1 — cannot drop them): the finished command's E and D and
# the cwd from our FIRST entry; then, from our LAST entry (after every entry of
# the user's), P;CommandMarks=1 when its checks pass and A. B stays in PS1:
# it is cosmetic. Per command line: C — from PS0 on bash >= 4.4, the DEBUG trap
# before that.
#
# P;CommandMarks=1 tells the runner this prompt's A means "nothing is running":
# the next typed command line will get a C first. It is sent ONLY when these
# checks pass at this prompt: bash >= 4.4 (PS0 exists), PS0 holds our C right
# now (re-prefixed if something reassigned PS0 — a `source ~/.bashrc` — unless
# PS0 is readonly), and both stdin (where the line is read) and stderr (where
# bash writes PS0) are still the pane: the terminal this shell started on, by
# its path, recorded once before the user's rc runs (`-ef`; not after an
# `exec 2>/dev/null`, `exec 2>>log`, `exec 2>/dev/pts/N` or `exec
# 0</dev/pts/N`, typed or in the rc; an `exec 2>&1` or the pane re-opened by
# its own path still passes). `exec 2>/dev/tty`
# fails it too, so it stops the announcement: the safe direction. With no
# recorded pane (`tty` failed) nothing is announced. The DEBUG trap is never
# announced: a user PROMPT_COMMAND's entries can spend its once-per-line C.
# Without the announcement the runner keeps the pane, which is always safe. It
# rides every prompt, so a runner pane that stopped parsing marks (remounted,
# or hidden) learns it again.
#
# Residual, documented rather than gated: a user PS0 is expanded with ours, and
# bash expands the WHOLE of PS0 before it writes any of it, so code in the
# user's part runs before our C reaches the pane (see the PS0 install below).
#
# The shared literal: `P;CommandMarks=1` is pinned against the webview's
# `COMMAND_MARKS_CAPABILITY` (src/components/terminal/shellIntegrationEvents.ts)
# by a source scan in src-tauri/src/session/spawn_prompt.rs.
__osc633_b=$'\033]633;B\007'
if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
    __qontinui_ps0_path=1
else
    __qontinui_ps0_path=0
fi

# ── Forks ───────────────────────────────────────────────────────────────────
# No prompt may print what plain bash would not, even where no fork succeeds (a
# task/PID ceiling). So every read below is either fork-free, or a command
# substitution inside `{ …; } 2>/dev/null`: bash's own `fork: retry` /
# `fork: Resource temporarily unavailable` text goes to that redirect.
#
# A fork that still fails, after bash's retries (about 15 s), throws bash back
# to its top level: it abandons PROMPT_COMMAND there and runs it again from its
# first entry, with `$?` now the failed fork's. Unguarded, the rerun forks
# again, and the shell never reaches its prompt (measured: base's `$(pwd)`
# loops the same way). So every fork made at a prompt sets __qontinui_forking
# while it runs. Still set when __qontinui_prompt_start comes round means the
# last round was abandoned: this round takes the finished command's status as
# that round saw it, makes no fork at all (__qontinui_no_fork), and sends
# only the marks it can read without one — each failed fork costs one stall,
# and nothing wrong is sent. bash >= 5.0 and 5.3 make most reads fork-free.
#
# Residual, where a fork is left (the E read on bash 5.1–5.2, the cwd under
# `set -o physical` or a PWD that does not name it, 4.4's readonly check of an
# unset PS0): after an abandoned round, bash's own `$?` — what PS1 and a bare
# `exit` see — is its fork-failure status (measured: 254) until the next
# command runs. The user's PROMPT_COMMAND entries and our D for the command get
# the real one; an empty line typed next reports bash's 254 in its D, as
# `echo $?` would.
__qontinui_forking=0
__qontinui_no_fork=0

# A variable's attributes (`r` readonly, `a` array) without a fork, bash >= 4.4.
# nounset is off for the read only (`local -`): bash >= 5.2 calls an unset name
# unbound there. In eval strings, so bash < 4.4 never parses them.
if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
    eval '__qontinui_attrs() {
        local -
        set +u
        eval "__qontinui_attrs_of=\${$1@a}"
    }'
fi

# Whether the variable named $1 is readonly. Fork-free on bash >= 5.0. bash
# 4.4's ${…@a} reads no attributes for a readonly name that has no value, so
# there an unset name, like every name below 4.4, is asked of `declare -p`.
if (( BASH_VERSINFO[0] >= 5 )); then
    __qontinui_is_readonly() {
        __qontinui_attrs "$1"
        [[ "${__qontinui_attrs_of}" == *r* ]]
    }
else
    __qontinui_is_readonly() {
        local decl attrs status
        if (( BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4 )) && eval '[[ -v $1 ]]'; then
            __qontinui_attrs "$1"
            attrs="${__qontinui_attrs_of}"
        else
            # A round that may not fork takes the name as readonly: nothing is
            # assigned, the safe direction.
            [ "${__qontinui_no_fork}" = 0 ] || return 0
            __qontinui_forking=1
            { decl="$(declare -p "$1" 2>/dev/null)"; } 2>/dev/null
            status=$?
            __qontinui_forking=0
            [ "${status}" = 0 ] || return 1
            attrs="${decl#declare -}"
            attrs="${attrs%% *}"
        fi
        [[ "${attrs}" == *r* ]]
    }
fi

# The newest history line and `pwd`, captured into __qontinui_history /
# __qontinui_pwd. bash >= 5.3 runs `${ …; }` in this shell: no fork (an eval
# string, so an older bash never parses it). Below 5.3 they are the silent
# command substitutions described above.
if (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 3) )); then
    eval '__qontinui_read_history() { { __qontinui_history=${ HISTTIMEFORMAT= builtin history 1; }; } 2>/dev/null; }
          __qontinui_read_pwd() { { __qontinui_pwd=${ builtin pwd; }; } 2>/dev/null; }'
else
    __qontinui_read_history() {
        [ "${__qontinui_no_fork}" = 0 ] || return 1
        local status
        __qontinui_forking=1
        { __qontinui_history="$(HISTTIMEFORMAT= builtin history 1)"; } 2>/dev/null
        status=$?
        __qontinui_forking=0
        return "${status}"
    }
    __qontinui_read_pwd() {
        [ "${__qontinui_no_fork}" = 0 ] || return 1
        local status
        __qontinui_forking=1
        { __qontinui_pwd="$(builtin pwd)"; } 2>/dev/null
        status=$?
        __qontinui_forking=0
        return "${status}"
    }
fi

# HISTCMD as our LAST entry left it: the number the next history entry will
# get, read after the user's entries ran (a `history -a; history -c; history -r`
# among them renumbers the list). Unset until the first prompt.
unset __qontinui_histcmd

# First PROMPT_COMMAND entry. `$?` is still the finished command's status, and
# is returned so the user's entries see it. E only when the typed line made a
# NEW history entry since our last entry (HISTCMD moved): an empty line, an
# `ignorespace`/`ignoredups` line, or a shell whose HISTCMD is gone sends no E —
# never a wrong one.
__qontinui_prompt_start() {
    local code=$?
    # The last round abandoned at a failed fork (see "Forks" above): its `$?`
    # was the finished command's; this one's is the fork's. No fork this round.
    if [ "${__qontinui_forking-0}" = 1 ]; then
        code="${__qontinui_code-${code}}"
        __qontinui_forking=0
        __qontinui_no_fork=1
    else
        __qontinui_no_fork=0
    fi
    __qontinui_code="${code}"
    # bash < 4.4: from here to the reset (our LAST entry), nothing that runs is
    # the typed command line, so the DEBUG trap's once-per-line E/C stays spent
    # — an empty line, or one that ran only a subshell, never armed it.
    __qontinui_in_command=1
    if [ "${__qontinui_ps0_path}" = "1" ] && [ -n "${__qontinui_histcmd-}" ] &&
        [ -n "${HISTCMD-}" ] && [ "${HISTCMD-}" != "${__qontinui_histcmd-}" ]; then
        __qontinui_history=""
        if __qontinui_read_history &&
            [[ "${__qontinui_history}" =~ ^[[:space:]]*[0-9]+[*[:space:]]+(.*)$ ]]; then
            local cmd="${BASH_REMATCH[1]//$'\n'/ }"
            __osc633 "E;${cmd//;/\\x3b}"
        fi
    fi
    __osc633 "D;${code}"
    # The cwd: $PWD while it still names this directory (`-ef .`) and `set -o
    # physical` is off, which is then what `pwd` prints — no fork. Otherwise
    # (physical; a PWD the user reassigned or unset) `pwd` itself. A PWD
    # reassigned to ANOTHER name of this directory is sent as the user set it.
    if [[ ! -o physical && "${PWD-}" == /* && "${PWD-}" -ef . ]]; then
        __osc633 "P;Cwd=${PWD}"
    else
        __qontinui_pwd=""
        if __qontinui_read_pwd && [ -n "${__qontinui_pwd}" ]; then
            __osc633 "P;Cwd=${__qontinui_pwd}"
        fi
    fi
    return "${code}"
}

# Last PROMPT_COMMAND entry, after every entry of the user's: record HISTCMD,
# then the capability (only if PS0 still carries our C, re-prefixing it where
# something reassigned PS0, and stdin and stderr — where bash writes PS0 — are
# still the pane) and A.
__qontinui_prompt_end() {
    local code=$?
    __qontinui_histcmd="${HISTCMD-}"
    if [ "${__qontinui_ps0_path}" = "1" ]; then
        case "${PS0-}" in
            *'\e]633;C\a'*) ;;
            *) __qontinui_is_readonly PS0 || PS0='\e]633;C\a'"${PS0-}" ;;
        esac
        case "${PS0-}" in
            *'\e]633;C\a'*)
                if [[ -n ${__qontinui_pane_tty-} && /dev/fd/0 -ef ${__qontinui_pane_tty-} &&
                    /dev/fd/2 -ef ${__qontinui_pane_tty-} ]]; then
                    __osc633 "P;CommandMarks=1"
                fi
                ;;
        esac
    fi
    __osc633 "A"
    return "${code}"
}

# Install: our start entry first, our end entry last, the user's (string or
# array form) untouched between. `&& :` / `|| :` run each entry where errexit
# is suspended, so a nonzero `$?` that the user's `set -e` exempts (`false &&
# true`) cannot end their shell at the next prompt; `&& :` keeps that `$?` for
# their entries. Newlines, not `;`, join the string form, so a user value
# ending in `;` stays valid. An array PROMPT_COMMAND is run element by element
# only on bash >= 5.1; earlier bash runs element [0] alone, so there it is
# wrapped as a string in element [0], exactly as that bash would run it.
if ! __qontinui_is_readonly PROMPT_COMMAND; then
    __qontinui_pc_start='__qontinui_prompt_start && :'
    __qontinui_pc_end='__qontinui_prompt_end || :'
    if (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1) )) &&
        __qontinui_attrs PROMPT_COMMAND && [[ "${__qontinui_attrs_of}" == *a* ]]; then
        PROMPT_COMMAND=("${__qontinui_pc_start}" "${PROMPT_COMMAND[@]}" "${__qontinui_pc_end}")
    elif [ -n "${PROMPT_COMMAND-}" ]; then
        PROMPT_COMMAND="${__qontinui_pc_start}"$'\n'"${PROMPT_COMMAND-}"$'\n'"${__qontinui_pc_end}"
    else
        PROMPT_COMMAND="${__qontinui_pc_start}"$'\n'"${__qontinui_pc_end}"
    fi

    # B (command ready) at the end of the prompt. \[...\] so bash doesn't count
    # the OSC bytes toward the prompt width.
    if ! __qontinui_is_readonly PS1; then
        PS1="${PS1-}\[${__osc633_b}\]"
    fi

    if [ "${__qontinui_ps0_path}" = "1" ]; then
        # PS0 is expanded once per command line read, after it is read and
        # before it runs — never for PROMPT_COMMAND's entries. `\e`/`\a` are
        # prompt escapes, so ours needs no command substitution (and no
        # promptvars). A user PS0 is kept, and its text follows our C — but
        # bash expands the WHOLE of PS0 before writing any of it, so the user's
        # part RUNS first, and a slow one delays our C. Measured with a 2 s
        # PS0: `$(sleep 2)` (bash 5.1, 5.3) and `${ sleep 2; }` (5.3) sent no C
        # for those 2 s while the shell had a `sleep` child, which the
        # runner's process read sees; a 5.3 function substitution of builtins
        # only (`${ while …; do :; done; }`) ran in-process, with no C and no
        # child — a residual neither witness sees. Not gated.
        if ! __qontinui_is_readonly PS0; then
            PS0='\e]633;C\a'"${PS0-}"
        fi
    else
        # bash < 4.4 has no PS0: E/C from the DEBUG trap, best effort and never
        # announced (see above). Our first entry marks the line spent and the
        # reset runs LAST, after the user's entries, so neither their commands
        # nor ours can take the trap's once-per-line E/C.
        __qontinui_in_command=1
        __qontinui_debug_trap() {
            local cmd="${BASH_COMMAND-}"
            case "${cmd}" in
                __qontinui_*|__osc633*) return 0 ;;
            esac
            if [ "${__qontinui_in_command-}" = "0" ]; then
                __qontinui_in_command=1
                __osc633 "E;${cmd//;/\\x3b}"
                __osc633 "C"
            fi
            return 0
        }
        __qontinui_reset_command() {
            __qontinui_in_command=0
        }
        PROMPT_COMMAND="${PROMPT_COMMAND-}"$'\n__qontinui_reset_command'
        # Never over a DEBUG trap of the user's own (bash-preexec and the like),
        # nor where `trap -p` could not be read (a silent fork, once).
        if { __qontinui_user_debug_trap="$(trap -p DEBUG)"; } 2>/dev/null &&
            [ -z "${__qontinui_user_debug_trap}" ]; then
            trap '__qontinui_debug_trap' DEBUG
        fi
    fi
fi
# An install-time fork that failed (bash abandoned the install above) leaves
# the flag set; the first prompt is a fresh round, not an abandoned one.
__qontinui_forking=0

# ── Claude Code runner context ─────────────────────────────────────────────
# Wrap the `claude` command so sessions launched from this terminal
# automatically know they are running inside the Qontinui Runner. The briefing
# text is the SINGLE SOURCE OF TRUTH rendered by the runner (Rust
# `terminal::runner_context`) and delivered via $QONTINUI_RUNNER_CONTEXT — this
# wrapper no longer authors its own copy. Fail-open: an empty value launches
# claude unmodified.
#
# When the runner composed a spawn file ($QONTINUI_RUNNER_CONTEXT_FILE: the same
# briefing plus the tenant's policy body — plan
# 2026-09-15-runner-policy-injection-off-sessionstart-hook-channel) and it still
# exists, it is passed via --append-system-prompt-file INSTEAD of the inline
# flag; Claude Code refuses both together, and a missing file is a fatal start,
# hence the existence check.
if [ "${QONTINUI_RUNNER_TERMINAL-}" = "1" ]; then
    claude() {
        case "${1:-}" in
            mcp|config|update|doctor|api-key)
                command claude "$@"
                ;;
            *)
                # Classify the caller's own APPEND flags — the only pair Claude
                # Code refuses together (`--system-prompt[-file]` starts beside
                # either append flag, so it never changes which flag we add):
                #   file   — a caller --append-system-prompt-file owns the append
                #            slot: ours is not added;
                #   inline — caller inline --append-system-prompt flag(s) only:
                #            ours joins as ANOTHER inline flag (the briefing, as
                #            before this plan), never the file;
                #   none   — ours alone, the composed file when it exists.
                # The scan stops at `--`: everything after it is the positional
                # prompt, and a prompt that reads like a flag is not one.
                # A caller REPLACEMENT prompt (`--system-prompt[-file]`) is
                # recorded separately: whether Claude Code still applies an
                # append file beside it is unverified, so our file is passed
                # but the delivered-policy marker is BLANKED and the policy
                # hook serves the full body.
                local __q_caller_prompt=none __q_replacement=0 __q_arg
                for __q_arg in "$@"; do
                    case "$__q_arg" in
                        --)
                            break ;;
                        --append-system-prompt-file|--append-system-prompt-file=*)
                            __q_caller_prompt=file ;;
                        --append-system-prompt|--append-system-prompt=*)
                            [ "$__q_caller_prompt" = "file" ] || __q_caller_prompt=inline ;;
                        --system-prompt|--system-prompt=*|--system-prompt-file|--system-prompt-file=*)
                            __q_replacement=1 ;;
                    esac
                done
                # Touch FIRST, then test: the touch re-arms a long-lived pane's
                # file against the 7-day age prune before the existence check
                # decides, which narrows the prune race. Fail-open — a file gone
                # after the touch takes the inline branch below.
                if [ "$__q_caller_prompt" = "none" ] && [ -n "${QONTINUI_RUNNER_CONTEXT_FILE:-}" ] \
                    && { touch -c -- "$QONTINUI_RUNNER_CONTEXT_FILE" 2>/dev/null; [ -f "${QONTINUI_RUNNER_CONTEXT_FILE}" ]; }; then
                    # The composed spawn file: briefing + the tenant's policy
                    # body. The delivered-policy marker rides along untouched —
                    # QONTINUI_POLICY_DELIVERED_FILE names exactly this file —
                    # unless a caller replacement prompt makes delivery unproven.
                    if [ "$__q_replacement" = "1" ]; then
                        QONTINUI_POLICY_DELIVERED_SHA= QONTINUI_POLICY_DELIVERED_FILE= \
                            command claude --append-system-prompt-file "$QONTINUI_RUNNER_CONTEXT_FILE" "$@"
                    else
                        command claude --append-system-prompt-file "$QONTINUI_RUNNER_CONTEXT_FILE" "$@"
                    fi
                elif [ "$__q_caller_prompt" != "file" ] && [ -n "${QONTINUI_RUNNER_CONTEXT:-}" ]; then
                    # Inline briefing (no file, it was pruned, or the caller
                    # brought inline flags of their own): no policy body reaches
                    # this child, so its delivered-policy marker is BLANKED —
                    # the policy hook then sends the full body.
                    QONTINUI_POLICY_DELIVERED_SHA= QONTINUI_POLICY_DELIVERED_FILE= \
                        command claude --append-system-prompt "$QONTINUI_RUNNER_CONTEXT" "$@"
                else
                    QONTINUI_POLICY_DELIVERED_SHA= QONTINUI_POLICY_DELIVERED_FILE= command claude "$@"
                fi
                ;;
        esac
    }
fi
