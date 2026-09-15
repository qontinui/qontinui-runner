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

# Saved exit code for D marker (captured before PROMPT_COMMAND clobbers $?)
__qontinui_last_exit=0

# PROMPT_COMMAND: emit D;<code> and P;Cwd=<path>
__qontinui_prompt_command() {
    __osc633 "D;${__qontinui_last_exit}"
    __osc633 "P;Cwd=$(pwd)"
}

# Prepend to any existing PROMPT_COMMAND
if [ -n "${PROMPT_COMMAND}" ]; then
    PROMPT_COMMAND="__qontinui_prompt_command;${PROMPT_COMMAND}"
else
    PROMPT_COMMAND="__qontinui_prompt_command"
fi

# Wrap PS1 with A (prompt start) and B (command ready) markers.
# Use \[...\] so bash doesn't count the OSC bytes toward prompt width.
__osc633_a=$'\033]633;A\007'
__osc633_b=$'\033]633;B\007'
PS1="\[${__osc633_a}\]${PS1}\[${__osc633_b}\]"

# Track whether we are inside a user-initiated command (not PROMPT_COMMAND internals)
__qontinui_in_command=0

# DEBUG trap: fires before each command about to execute.
# We only want to emit E/C once per user-typed line, not for every sub-command.
__qontinui_debug_trap() {
    # Capture exit code before anything else changes it
    __qontinui_last_exit=$?
    local cmd="${BASH_COMMAND}"
    # Ignore internal housekeeping commands
    case "${cmd}" in
        __qontinui_*|__osc633*) return ;;
    esac
    # Only emit E/C for the first command of a new user input (not sub-commands)
    if [ "${__qontinui_in_command}" = "0" ]; then
        __qontinui_in_command=1
        local escaped="${cmd//;/\\x3b}"
        __osc633 "E;${escaped}"
        __osc633 "C"
    fi
}

# Reset in_command flag when PROMPT_COMMAND runs (a new prompt is about to appear)
__qontinui_reset_command() {
    __qontinui_in_command=0
}

# Prepend reset to PROMPT_COMMAND so in_command is cleared each new prompt
PROMPT_COMMAND="__qontinui_reset_command;${PROMPT_COMMAND}"

trap '__qontinui_debug_trap' DEBUG

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
                # Classify the caller's own system-prompt flags. Claude Code
                # refuses the inline and file flags together but accepts
                # repeated inline flags, so:
                #   file  — a caller --append-system-prompt-file or
                #           --system-prompt[-file] owns the prompt: ours is not
                #           added at all (either of ours could stop the launch
                #           or override theirs);
                #   inline — caller inline --append-system-prompt flag(s) only:
                #           ours joins as ANOTHER inline flag (the briefing, as
                #           before this plan), never the file;
                #   none  — ours alone, the composed file when it exists.
                local __q_caller_prompt=none __q_arg
                for __q_arg in "$@"; do
                    case "$__q_arg" in
                        --append-system-prompt-file|--append-system-prompt-file=*|--system-prompt|--system-prompt=*|--system-prompt-file|--system-prompt-file=*)
                            __q_caller_prompt=file; break ;;
                        --append-system-prompt|--append-system-prompt=*)
                            __q_caller_prompt=inline ;;
                    esac
                done
                if [ "$__q_caller_prompt" = "none" ] && [ -n "${QONTINUI_RUNNER_CONTEXT_FILE:-}" ] \
                    && [ -f "${QONTINUI_RUNNER_CONTEXT_FILE}" ]; then
                    # The composed spawn file: briefing + the tenant's policy
                    # body. The delivered-policy marker rides along untouched —
                    # QONTINUI_POLICY_DELIVERED_FILE names exactly this file.
                    command claude --append-system-prompt-file "$QONTINUI_RUNNER_CONTEXT_FILE" "$@"
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
