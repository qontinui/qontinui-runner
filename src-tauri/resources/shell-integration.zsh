#!/usr/bin/env zsh
# Qontinui Shell Integration for Zsh (OSC 633 / VS Code compatible)
#
# Loaded via ZDOTDIR: the runner points $ZDOTDIR at a temp dir whose `.zshrc`
# is this file, so zsh sources it instead of ~/.zshrc. zsh has no `--rcfile`
# flag (that's bash) — ZDOTDIR is the supported mechanism.
#
# Because ZDOTDIR redirects ALL of zsh's startup files away from $HOME, we must
# re-source the user's real config explicitly to preserve their PATH/aliases/etc.

# Restore the user's real zsh environment (in normal startup order). These were
# skipped because ZDOTDIR points away from $HOME.
[ -f "$HOME/.zshenv" ]   && source "$HOME/.zshenv"
[ -f "$HOME/.zprofile" ] && source "$HOME/.zprofile"
[ -f "$HOME/.zshrc" ]    && source "$HOME/.zshrc"

# Guard: only install the integration hooks once.
if [ "${QONTINUI_INTEGRATION}" = "1" ]; then
    return 0 2>/dev/null
fi
export QONTINUI_INTEGRATION=1

autoload -Uz add-zsh-hook

# OSC 633 helper — write directly to /dev/tty so it never pollutes stdout.
__qontinui_osc633() { printf '\033]633;%s\007' "$1" > /dev/tty }

# precmd: fires before each prompt. Emit D;<exit-code> and P;Cwd=<path>.
__qontinui_precmd() {
    local code=$?
    __qontinui_osc633 "D;${code}"
    __qontinui_osc633 "P;Cwd=${PWD}"
}

# preexec: fires before a user-typed command runs. Emit E;<cmd> and C.
__qontinui_preexec() {
    local cmd="$1"
    local escaped="${cmd//;/\\x3b}"
    __qontinui_osc633 "E;${escaped}"
    __qontinui_osc633 "C"
}

add-zsh-hook precmd __qontinui_precmd
add-zsh-hook preexec __qontinui_preexec

# Wrap PROMPT with A (prompt start) and B (command ready) markers. %{...%} marks
# them zero-width so zsh doesn't miscount the prompt length.
PROMPT="%{$(printf '\033]633;A\007')%}${PROMPT}%{$(printf '\033]633;B\007')%}"

# ── Claude Code runner context ─────────────────────────────────────────────
# Wrap `claude` so sessions launched from this terminal know they run inside
# the Qontinui Runner (mirrors shell-integration.bash). The briefing text is the
# SINGLE SOURCE OF TRUTH rendered by the runner (Rust `terminal::runner_context`)
# and delivered via $QONTINUI_RUNNER_CONTEXT. Fail-open: an empty value launches
# claude unmodified.
if [ "${QONTINUI_RUNNER_TERMINAL}" = "1" ]; then
    claude() {
        case "${1:-}" in
            mcp|config|update|doctor|api-key)
                command claude "$@"
                ;;
            *)
                if [ -n "${QONTINUI_RUNNER_CONTEXT}" ]; then
                    command claude --append-system-prompt "$QONTINUI_RUNNER_CONTEXT" "$@"
                else
                    command claude "$@"
                fi
                ;;
        esac
    }
fi
