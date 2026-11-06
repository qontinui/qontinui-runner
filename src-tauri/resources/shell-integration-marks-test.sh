#!/usr/bin/env bash
# shell-integration-marks-test.sh — the bash and zsh integrations' prompt-state
# marks, observed on a real pty.
#
# The runner replaces a terminal pane ("Move them") only when its shell's last
# OSC 633 mark is a prompt (A) AND that prompt announced `P;CommandMarks=1`,
# i.e. that the next typed command line gets a command-start mark (C) first.
# That promise must hold whatever the user's own shell setup does, the setup
# itself must keep working, and a prompt that cannot keep the promise must not
# make it. Each case installs a user rc, then types
#
#     [case-specific lines]  false && true  printf 'OUT%s\n' 42  <empty line>  exit
#
# and asserts on the mark stream, read as tokens M (P;CommandMarks=1), A, C,
# E (any 633;E) and O (the command's OUTPUT, OUT42 — its text has `OUT%s`):
#
#   - the case's expected run of tokens appears in order. For a case whose
#     prompts can keep the promise it is, in bash, " M A C O E M A M A ": the
#     capability before A at every prompt; the printf's C before its output;
#     its E (from history, at the next prompt) after it; the empty line gets no
#     C and no E; the next prompt again M then A. zsh sends E from preexec,
#     before C, so there it is " M A E C O M A M A ";
#   - at most the case's number of M tokens: a prompt whose checks cannot see
#     its C path in place and reaching the pane (a readonly PS0, a stdin or
#     stderr that is no longer the pane, no integration at all), or that sees a
#     registered user hook ahead of it (a zsh hook before ours, a
#     `zshaddhistory` hook, a DEBUG trap), must not announce;
#   - no E is sent twice: a seeded history, an empty line, a PROMPT_COMMAND
#     entry or a history file another shell appended to never re-sends one; the
#     printf has its one E wherever the expected run has one;
#   - the shell survived to `exit` (the output is there at all), and reported
#     no error (`unbound variable` under `set -u`, a `readonly variable`, …).
#
# A bash case also seeds ~/.bash_history with the very command it will type, so
# an E read from a stale history entry would show. On bash 4.4–5.0, HISTCMD
# stays 1 inside PROMPT_COMMAND, so the integration sends no E at all there
# (never a wrong one) and the bash cases expect their runs without E.
#
# Every shell runs in a pinned environment (`env -i`, plus what a runner pane
# sets), so the caller's — a runner pane exports QONTINUI_INTEGRATION and
# QONTINUI_RUNNER_TERMINAL — cannot change the result. The zsh cases SKIP
# where zsh is not installed.
#
# Run: bash src-tauri/resources/shell-integration-marks-test.sh
# Exit 0 = pass (or SKIP without util-linux script(1)), 1 = a case failed.
# Driven from CI by src-tauri/src/session/spawn_prompt.rs
# `script_tests::shell_integration_marks_every_typed_command`, whose budget is
# sized from the number of `run_case` / `run_case_input_moves` / `run_zsh_case`
# lines below and the per-case bound.
set -u

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
integ="${SHELL_INTEGRATION_BASH:-${here}/shell-integration.bash}"
zinteg="${SHELL_INTEGRATION_ZSH:-${here}/shell-integration.zsh}"

# util-linux script(1) (`-c`, `-e`) and coreutils timeout(1): a BSD script(1)
# (macOS) takes a different argv.
if ! script --version 2>/dev/null | grep -q util-linux; then
    echo "SKIP: util-linux script(1) is not available"
    exit 0
fi
if ! command -v timeout >/dev/null 2>&1; then
    echo "SKIP: timeout(1) is not available"
    exit 0
fi
if ! (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
    echo "SKIP: bash ${BASH_VERSION} predates PS0; the integration announces no command marks"
    exit 0
fi

tmp="$(mktemp -d)"
keep=0
other_holder=""
cleanup() {
    if [ -n "${other_holder}" ]; then
        kill "${other_holder}" 2>/dev/null
        wait "${other_holder}" 2>/dev/null
    fi
    exec 9>&-
    # A failed case's transcripts stay for the reader.
    [ "${keep}" = 1 ] || rm -rf "${tmp}"
}
trap cleanup EXIT
esc=$'\033'
bel=$'\007'
typed="printf 'OUT%s\\n' 42"
failures=0
# The runs every prompt that can keep the promise produces.
marks=" M A C O E M A M A "
zmarks=" M A E C O M A M A "
# bash 4.4–5.0: no E (see the header).
bash_sends_e=1
if (( BASH_VERSINFO[0] == 4 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] == 0) )); then
    bash_sends_e=0
fi

fail() {
    echo "FAIL $1"
    failures=$((failures + 1))
    keep=1
}

# A second terminal, held open for the whole run, for the cases that point the
# shell's stdin and/or stderr at a terminal that is not its pane. Its holder's
# script(1) reads the fifo on fd 9 (opened here, read-write, so the holder's
# stdin never ends) and types what arrives into that terminal: it is also the
# input channel of a shell whose stdin has moved there (run_case_input_moves).
other_tty=""
start_other_terminal() {
    local fifo="${tmp}/other-terminal-stdin" ttyfile="${tmp}/other-terminal" i
    mkfifo "${fifo}" || return 1
    exec 9<>"${fifo}"
    SHELL=/bin/sh timeout -k 2 300 script -q -e -c "tty >'${ttyfile}'; exec sleep 290" /dev/null \
        <"${fifo}" >/dev/null 2>&1 &
    other_holder=$!
    for i in {1..100}; do
        [ -s "${ttyfile}" ] && break
        sleep 0.1
    done
    other_tty="$(cat "${ttyfile}" 2>/dev/null)"
    case "${other_tty}" in
        /dev/*) return 0 ;;
    esac
    return 1
}

# check_case <name> <home> <output> <expected run> <most M tokens, or any> <printf E count>
check_case() {
    local name="$1" home="$2" out="$3" want="$4" max_m="$5" want_e="$6"
    printf '%s' "${out}" >"${home}/transcript"

    # The marks and the output token, in stream order, one letter each.
    local seq
    seq="$(printf '%s' "${out}" |
        grep -ao -e "${esc}]633;P;CommandMarks=1${bel}" -e "${esc}]633;A${bel}" \
            -e "${esc}]633;C${bel}" -e "${esc}]633;E;[^${bel}]*${bel}" -e "OUT42" |
        sed -e "s/^${esc}]633;P;CommandMarks=1${bel}\$/M/" -e "s/^${esc}]633;A${bel}\$/A/" \
            -e "s/^${esc}]633;C${bel}\$/C/" -e "s/^${esc}]633;E;.*/E/" -e 's/^OUT42$/O/' |
        tr '\n' ' ')"

    local why=""
    case " ${seq} " in
        *"${want}"*) ;;
        *) why="${why} expected '${want}' in the mark stream, got '${seq}';" ;;
    esac
    if [ "${max_m}" != "any" ]; then
        # One count per M token, however many sit side by side.
        local token m_count=0
        for token in ${seq}; do
            if [ "${token}" = M ]; then m_count=$((m_count + 1)); fi
        done
        if [ "${m_count}" -gt "${max_m}" ]; then
            why="${why} P;CommandMarks=1 was announced ${m_count} times, at most ${max_m} allowed;"
        fi
    fi
    # The user's shell must not see the integration fail: no shell error at
    # all (an unbound variable under `set -u` aborts the rest of the rc).
    case "${out}" in
        *"unbound variable"* | *"readonly variable"* | *"read-only variable"* | *"syntax error"* | \
            *"command not found"* | *"parameter not set"* | *"parse error"* | *"bad substitution"* | \
            *"fork failed"* | *"Resource temporarily unavailable"*)
            why="${why} the shell reported an error;"
            ;;
    esac
    # Every line this test types is distinct, so a repeated E is a re-send.
    local repeated
    repeated="$(printf '%s' "${out}" | grep -ao -e "${esc}]633;E;[^${bel}]*" | sort | uniq -d |
        sed -e "s/^${esc}]633;//" | tr '\n' ' ')"
    if [ -n "${repeated}" ]; then
        why="${why} an E was sent more than once: ${repeated};"
    fi
    local e_count
    e_count="$(printf '%s' "${out}" | grep -ao -e "${esc}]633;E;printf 'OUT%s" | wc -l)"
    if [ "${e_count}" -ne "${want_e}" ]; then
        why="${why} the printf's E was sent ${e_count} times, not ${want_e};"
    fi

    if [ -n "${why}" ]; then
        fail "${name}:${why} (transcript: ${home}/transcript)"
    else
        echo "ok   ${name}"
    fi
}

# The lines a case types, one per prompt: its own first, then the common tail.
type_lines() {
    local line
    for line in "$@" "false && true" "${typed}" "" "exit"; do
        printf '%s\n' "${line}"
        sleep 0.3
    done
}

# run_case <name> <expected run> <most M tokens, or any> <bashrc body> [lines typed first...]
run_case() {
    local name="$1" want="$2" max_m="$3" rc="$4"
    shift 4
    local home="${tmp}/${name}"
    mkdir -p "${home}"
    printf '%s\n' "${rc}" >"${home}/.bashrc"
    # The command this case types is already the newest history entry.
    printf '%s\n' "${typed}" >"${home}/.bash_history"
    # The environment is a runner pane's, and nothing of the caller's.
    local out
    out="$(type_lines "$@" |
        env -i PATH="${PATH}" HOME="${home}" TERM=dumb QONTINUI_RUNNER_TERMINAL=1 \
            timeout -k 2 20 script -q -e -c "bash --rcfile '${integ}' -i" /dev/null 2>&1)"
    check_bash_case "${name}" "${home}" "${out}" "${want}" "${max_m}"
}

# bash sends the printf's E from history at the next prompt, after its output —
# where it sends E at all.
check_bash_case() {
    local name="$1" home="$2" out="$3" want="$4" max_m="$5" want_e=0
    if [ "${bash_sends_e}" = 0 ]; then
        want="${want// E/}"
    fi
    case "${want}" in
        *"O E"*) want_e=1 ;;
    esac
    check_case "${name}" "${home}" "${out}" "${want}" "${max_m}" "${want_e}"
}

# run_case_input_moves <name> <expected run> <most M tokens> <bashrc body> <redirect line, or "">
# Like run_case, but the shell's stdin moves to the second terminal — by the
# redirect line typed into the pane, or (with "") by the rc itself — so the
# common tail is typed THERE, through fd 9.
run_case_input_moves() {
    local name="$1" want="$2" max_m="$3" rc="$4" redirect="$5"
    local home="${tmp}/${name}"
    mkdir -p "${home}"
    printf '%s\n' "${rc}" >"${home}/.bashrc"
    printf '%s\n' "${typed}" >"${home}/.bash_history"
    local out
    out="$({
        if [ -n "${redirect}" ]; then
            printf '%s\n' "${redirect}"
        fi
        sleep 0.6
        type_lines >&9
        sleep 1
    } | env -i PATH="${PATH}" HOME="${home}" TERM=dumb QONTINUI_RUNNER_TERMINAL=1 \
        timeout -k 2 20 script -q -e -c "bash --rcfile '${integ}' -i" /dev/null 2>&1)"
    check_bash_case "${name}" "${home}" "${out}" "${want}" "${max_m}"
}

# run_zsh_case <name> <expected run> <most M tokens, or any> <~/.zshrc body> [lines typed first...]
# The runner points ZDOTDIR at a directory whose .zshrc is the integration, and
# the integration sources the user's ~/.zshenv, ~/.zprofile and ~/.zshrc. The
# case's ZDOTDIR also gets a .zshenv that turns GLOBAL_RCS off, so the host's
# /etc/zsh/zshrc (compinit, keymaps) is not part of what is measured.
run_zsh_case() {
    local name="$1" want="$2" max_m="$3" rc="$4"
    shift 4
    if [ -z "${zsh_bin}" ]; then
        echo "SKIP ${name}: zsh is not installed"
        return 0
    fi
    # Root is exempt from `ulimit -u`, so a case that sets one needs a uid it binds.
    case "${rc}" in
        *"ulimit -u"*)
            if [ "${EUID}" = 0 ]; then
                echo "SKIP ${name}: root is exempt from ulimit -u"
                return 0
            fi
            ;;
    esac
    local home="${tmp}/${name}"
    mkdir -p "${home}/zdotdir"
    printf '%s\n' "${rc}" >"${home}/.zshrc"
    cp "${zinteg}" "${home}/zdotdir/.zshrc"
    printf '%s\n' 'unsetopt GLOBAL_RCS' >"${home}/zdotdir/.zshenv"
    local out
    out="$(type_lines "$@" |
        env -i PATH="${PATH}" HOME="${home}" ZDOTDIR="${home}/zdotdir" TERM=dumb \
            QONTINUI_RUNNER_TERMINAL=1 \
            timeout -k 2 20 script -q -e -c "'${zsh_bin}' -i" /dev/null 2>&1)"
    # zsh sends the printf's E from preexec, before its C, whenever our hook runs.
    local want_e=0
    case "${want}" in
        *"E C O"*) want_e=1 ;;
    esac
    check_case "${name}" "${home}" "${out}" "${want}" "${max_m}" "${want_e}"
}
zsh_bin="$(command -v zsh 2>/dev/null || true)"

# ── bash ────────────────────────────────────────────────────────────────────
# Every form of a user PROMPT_COMMAND, and one that rewrites PS1 every prompt.
run_case no-prompt-command "${marks}" any 'PS1="$ "'
run_case string-prompt-command "${marks}" any 'PS1="$ "; PROMPT_COMMAND="history -a"'
run_case string-ending-in-semicolon "${marks}" any 'PS1="$ "; PROMPT_COMMAND="history -a; true;"'
run_case array-prompt-command "${marks}" any 'PS1="$ "; PROMPT_COMMAND=(true "history -a")'
run_case rewrites-ps1-every-prompt "${marks}" any 'PS1="$ "; PROMPT_COMMAND='"'"'history -a; PS1="\$ "'"'"
# `set -u` / `set -e` in the user's rc survive, with and without their own
# PROMPT_COMMAND (`false && true` leaves `$?` nonzero for the entries).
run_case set-u "${marks}" any 'PS1="$ "; set -u; PROMPT_COMMAND="history -a"'
run_case set-u-outside-a-runner-pane "${marks}" any 'PS1="$ "; unset QONTINUI_RUNNER_TERMINAL; set -u; PROMPT_COMMAND="history -a"'
run_case set-e "${marks}" any 'PS1="$ "; set -e; PROMPT_COMMAND="history -a"'
run_case set-e-no-prompt-command "${marks}" any 'PS1="$ "; set -e'
# PS0 reassigned at runtime (a re-sourced rc does this): the next prompt must
# re-prefix our C before it may announce the capability.
run_case ps0-reassigned "${marks}" any 'PS1="$ "; PROMPT_COMMAND="history -a"' 'PS0=""'
# A readonly variable is left alone, with no error: a readonly PS1 only loses
# B; a readonly PS0 cannot carry our C, so no prompt announces; a readonly
# PROMPT_COMMAND means nothing is installed at all.
run_case readonly-ps1 "${marks}" any 'readonly PS1="$ "; PROMPT_COMMAND="history -a"'
run_case readonly-ps0 " A E A O E A A " 0 'PS1="$ "; readonly PS0=""'
run_case readonly-prompt-command " O " 0 'PS1="$ "; readonly PROMPT_COMMAND="history -a"'
# bash writes PS0 to stderr and reads lines from stdin: once either is not the
# pane — not a terminal at all, or ANOTHER terminal (which `[ -t 2 ]` alone
# accepts), or both moved to another terminal together (which comparing them
# with each other accepts) — only the prompt before the redirect announces.
# stderr re-opened on the pane by its own path still announces. (`exec 2>&1`
# has no case: under script(1) fds 0, 1 and 2 already share one open file, so
# it changes nothing a case could see.)
run_case stderr-not-a-terminal " A O E A A " 1 'PS1="$ "' 'exec 2>/dev/null'
start_other_terminal || fail "stderr-on-another-terminal: could not hold a second terminal open"
run_case stderr-on-another-terminal " A O E A A " 1 'PS1="$ "' "exec 2>${other_tty:-/dev/null/no-second-terminal}"
run_case_input_moves stdin-and-stderr-on-another-terminal " A O E A A " 1 'PS1="$ "' \
    "exec 0<${other_tty:-/dev/null/no-second-terminal} 2>${other_tty:-/dev/null/no-second-terminal}"
# stdin alone moved: the C still reaches the pane, but the lines are no longer
# the pane's, so the prompt is not announced either (the safe direction).
run_case_input_moves stdin-on-another-terminal " A C O E A A " 1 'PS1="$ "' \
    "exec 0<${other_tty:-/dev/null/no-second-terminal}"
# The same redirect made by the user's rc, before the first prompt: the pane is
# the terminal the shell STARTED on, recorded before the rc runs, so no prompt
# announces.
run_case_input_moves rc-moves-stdin-and-stderr " A E A O E A A " 0 \
    "PS1='\$ '; exec 0<${other_tty:-/dev/null/no-second-terminal} 2>${other_tty:-/dev/null/no-second-terminal}" ""
run_case stderr-reopened-by-its-path "${marks}" any 'PS1="$ "' 'exec 2>"$(tty)"'
# A shared history (`history -a; history -c; history -r` every prompt) that
# another shell appended to: the reload renumbers the list, and the empty line
# after it must not re-send the previous line's E.
run_case shared-history "${marks}" any 'PS1="$ "; PROMPT_COMMAND="history -a; history -c; history -r"' \
    'echo from-other-session >> ~/.bash_history' ''

# ── zsh ─────────────────────────────────────────────────────────────────────
run_zsh_case zsh-no-user-hooks "${zmarks}" any 'PS1="$ "'
# The user's options stay out of the integration's own checks.
run_zsh_case zsh-ksh-arrays "${zmarks}" any 'PS1="$ "; setopt KSH_ARRAYS'
run_zsh_case zsh-nounset "${zmarks}" any 'PS1="$ "; setopt NO_UNSET'
# A hook added AFTER ours runs after our C: still announced.
run_zsh_case zsh-hook-appended-later "${zmarks}" any 'PS1="$ "; h() { : }' 'add-zsh-hook preexec h'
# A hook put AHEAD of ours later (a deferred plugin) runs before our C, and a
# user `preexec` function always does: neither prompt may announce.
run_zsh_case zsh-hook-prepended-later " E C O A A " 1 'PS1="$ "; slow() { : }' \
    'preexec_functions=(slow $preexec_functions)'
run_zsh_case zsh-preexec-function " E C O A A " 0 'PS1="$ "; preexec() { : }'
# zsh runs zshaddhistory hooks, and a DEBUG trap fires before every command
# (our preexec's first one included), ahead of our C: never announced.
run_zsh_case zsh-addhistory-hook " E C O A A " 0 'PS1="$ "; autoload -Uz add-zsh-hook; h() { return 0 }; add-zsh-hook zshaddhistory h'
run_zsh_case zsh-addhistory-function " E C O A A " 0 'PS1="$ "; zshaddhistory() { return 0 }'
run_zsh_case zsh-debug-trap " E C O A A " 0 "PS1='\$ '; trap ':' DEBUG"
run_zsh_case zsh-trapdebug-function " E C O A A " 0 'PS1="$ "; TRAPDEBUG() { : }'
# A user function named `trap` must not hide the trap from the read.
run_zsh_case zsh-trap-shadowed-by-a-function " E C O A A " 0 "PS1='\$ '; trap ':' DEBUG; trap() { : }"
# At a process ceiling no fork succeeds, so the DEBUG trap cannot be read: the
# prompt must stay silent (no error text), still send A, and not announce.
run_zsh_case zsh-at-a-process-ceiling " E C O A A " 0 'PS1="$ "; ulimit -u 1'
# Our hook removed: no C at all, so no announcement.
run_zsh_case zsh-our-hook-removed " O A A " 1 'PS1="$ "' 'add-zsh-hook -d preexec __qontinui_preexec'

if [ "${failures}" -ne 0 ]; then
    echo "${failures} case(s) failed (transcripts kept under ${tmp})"
    exit 1
fi
echo "all cases passed"
