#!/usr/bin/env bash
# Qontinui Shell Integration for Bash (OSC 633 / VS Code compatible)
# Used as --rcfile so the shell sources it instead of ~/.bashrc.

# Guard: only install once
if [ "${QONTINUI_INTEGRATION}" = "1" ]; then
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
# Per prompt: D;<code>, P;Cwd=<path>, P;CommandMarks=1 (bash >= 4.4 only, see
# below), then A — all from PROMPT_COMMAND, so a PROMPT_COMMAND that rewrites
# PS1 (starship, __git_ps1) cannot drop them. B stays in PS1: it is cosmetic.
# Per command line: C (and nothing for PROMPT_COMMAND's own entries) — from
# PS0 on bash >= 4.4, the DEBUG trap before that.
#
# P;CommandMarks=1 tells the runner a prompt mark (A) here means "nothing is
# running": it is sent only where every typed command line gets a C, i.e. only
# on the PS0 path. The DEBUG trap cannot promise that — once the user has a
# PROMPT_COMMAND of their own (history -a, direnv, zoxide) its entries spend the
# trap's once-per-line C before the typed command runs. It rides every prompt,
# not just the first: a runner pane that stopped parsing marks (remounted, or
# hidden) learns it again at the next prompt.
#
# The shared literal: `P;CommandMarks=1` is pinned against the webview's
# `COMMAND_MARKS_CAPABILITY` (src/components/terminal/shellIntegrationEvents.ts)
# by a source scan in src-tauri/src/session/spawn_prompt.rs.
__osc633_b=$'\033]633;B\007'
if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
    __qontinui_command_marks=1
else
    __qontinui_command_marks=0
fi

# The history number of the last command line E was sent for (PS0 path).
# Unset until the first prompt records it, so that prompt never re-sends the
# last command of a previous session (the history file loads after this rc).
unset __qontinui_last_histnum

# First PROMPT_COMMAND entry: the finished command's E (PS0 path: read back
# from history, so PROMPT_COMMAND entries never masquerade as it), its D, the
# cwd and the capability. `$?` is still the command's status here.
__qontinui_prompt_start() {
    local code=$?
    if [ "${__qontinui_command_marks}" = "1" ]; then
        local line num=0 cmd=
        line=$(HISTTIMEFORMAT= builtin history 1)
        if [[ "${line}" =~ ^[[:space:]]*([0-9]+)[*[:space:]]+(.*)$ ]]; then
            num="${BASH_REMATCH[1]}"
            cmd="${BASH_REMATCH[2]//$'\n'/ }"
        fi
        if [ -z "${__qontinui_last_histnum+set}" ]; then
            __qontinui_last_histnum="${num}"
        elif [ "${num}" != "${__qontinui_last_histnum}" ]; then
            __qontinui_last_histnum="${num}"
            __osc633 "E;${cmd//;/\\x3b}"
        fi
    fi
    __osc633 "D;${code}"
    __osc633 "P;Cwd=$(pwd)"
    if [ "${__qontinui_command_marks}" = "1" ]; then
        __osc633 "P;CommandMarks=1"
    fi
    return "${code}"
}

# Last PROMPT_COMMAND entry: A, after every entry of the user's own.
__qontinui_prompt_end() {
    local code=$?
    __osc633 "A"
    return "${code}"
}

# Install: our start entry first, our end entry last, the user's (string or
# array form) untouched between. Newlines, not `;`, join the string form, so a
# user value ending in `;` stays valid.
if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
    PROMPT_COMMAND=("__qontinui_prompt_start" "${PROMPT_COMMAND[@]}" "__qontinui_prompt_end")
elif [ -n "${PROMPT_COMMAND}" ]; then
    PROMPT_COMMAND=$'__qontinui_prompt_start\n'"${PROMPT_COMMAND}"$'\n__qontinui_prompt_end'
else
    PROMPT_COMMAND=$'__qontinui_prompt_start\n__qontinui_prompt_end'
fi

# B (command ready) at the end of the prompt. \[...\] so bash doesn't count the
# OSC bytes toward the prompt width.
PS1="${PS1}\[${__osc633_b}\]"

if [ "${__qontinui_command_marks}" = "1" ]; then
    # PS0 is expanded once per command line read, after it is read and before
    # it runs — never for PROMPT_COMMAND's entries. `\e`/`\a` are prompt
    # escapes, so this needs no command substitution (and no promptvars).
    PS0='\e]633;C\a'"${PS0}"
else
    # bash < 4.4 has no PS0: E/C from the DEBUG trap, best effort and never
    # announced (see above).
    __qontinui_in_command=1
    __qontinui_debug_trap() {
        local cmd="${BASH_COMMAND}"
        case "${cmd}" in
            __qontinui_*|__osc633*) return ;;
        esac
        if [ "${__qontinui_in_command}" = "0" ]; then
            __qontinui_in_command=1
            __osc633 "E;${cmd//;/\\x3b}"
            __osc633 "C"
        fi
    }
    __qontinui_reset_command() {
        __qontinui_in_command=0
    }
    PROMPT_COMMAND=$'__qontinui_reset_command\n'"${PROMPT_COMMAND}"
    trap '__qontinui_debug_trap' DEBUG
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
if [ "${QONTINUI_RUNNER_TERMINAL}" = "1" ]; then
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
