#!/usr/bin/env bash
# Qontinui Shell Integration for Bash (OSC 633 / VS Code compatible)
# Used as --rcfile so the shell sources it instead of ~/.bashrc.

# Guard: only install once
if [ "${QONTINUI_INTEGRATION-}" = "1" ]; then
    [ -f ~/.bashrc ] && source ~/.bashrc
    return 0 2>/dev/null || exit 0
fi
export QONTINUI_INTEGRATION=1

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
# PS0 is readonly), and stderr, where bash writes PS0, is the terminal the
# shell reads from: a terminal (`[ -t 2 ]`, not after `exec 2>/dev/null` or
# `exec 2>>log`) and the same file as stdin (`-ef`, not after `exec
# 2>/dev/pts/N`, another terminal). `exec 2>/dev/tty` fails the second check
# too, so it stops the announcement: the safe direction. The DEBUG trap is never
# announced: a user PROMPT_COMMAND's entries can spend its once-per-line C.
# Without the announcement the runner keeps the pane, which is always safe. It
# rides every prompt, so a runner pane that stopped parsing marks (remounted,
# or hidden) learns it again.
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

# Whether the variable named $1 is readonly (a fork; install time and the rare
# PS0 re-prefix only).
__qontinui_is_readonly() {
    local decl attrs
    decl="$(declare -p "$1" 2>/dev/null)" || return 1
    attrs="${decl#declare -}"
    attrs="${attrs%% *}"
    [[ "${attrs}" == *r* ]]
}

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
    # bash < 4.4: from here to the reset (our LAST entry), nothing that runs is
    # the typed command line, so the DEBUG trap's once-per-line E/C stays spent
    # — an empty line, or one that ran only a subshell, never armed it.
    __qontinui_in_command=1
    if [ "${__qontinui_ps0_path}" = "1" ] && [ -n "${__qontinui_histcmd-}" ] &&
        [ -n "${HISTCMD-}" ] && [ "${HISTCMD-}" != "${__qontinui_histcmd-}" ]; then
        local line
        line="$(HISTTIMEFORMAT= builtin history 1)"
        if [[ "${line}" =~ ^[[:space:]]*[0-9]+[*[:space:]]+(.*)$ ]]; then
            local cmd="${BASH_REMATCH[1]//$'\n'/ }"
            __osc633 "E;${cmd//;/\\x3b}"
        fi
    fi
    __osc633 "D;${code}"
    __osc633 "P;Cwd=$(pwd)"
    return "${code}"
}

# Last PROMPT_COMMAND entry, after every entry of the user's: record HISTCMD,
# then the capability (only if PS0 still carries our C, re-prefixing it where
# something reassigned PS0, and stderr — where bash writes PS0 — is the
# terminal stdin is) and A.
__qontinui_prompt_end() {
    local code=$?
    __qontinui_histcmd="${HISTCMD-}"
    if [ "${__qontinui_ps0_path}" = "1" ]; then
        case "${PS0-}" in
            *'\e]633;C\a'*) ;;
            *) __qontinui_is_readonly PS0 || PS0='\e]633;C\a'"${PS0-}" ;;
        esac
        case "${PS0-}" in
            *'\e]633;C\a'*) if [ -t 2 ] && [[ /dev/fd/2 -ef /dev/fd/0 ]]; then __osc633 "P;CommandMarks=1"; fi ;;
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
    if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]] &&
        (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1) )); then
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
        # prompt escapes, so this needs no command substitution (and no
        # promptvars). A user PS0 is kept, after ours.
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
        # Never over a DEBUG trap of the user's own (bash-preexec and the like).
        if [ -z "$(trap -p DEBUG)" ]; then
            trap '__qontinui_debug_trap' DEBUG
        fi
    fi
fi

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
