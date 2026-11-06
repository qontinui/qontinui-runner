#!/usr/bin/env bash
# shell-integration-marks-test.sh — the bash integration's prompt-state marks,
# observed on a real pty.
#
# The runner replaces a terminal pane ("Move them") only when its shell's last
# OSC 633 mark is a prompt (A) AND that prompt announced `P;CommandMarks=1`,
# i.e. that the next typed command line gets a command-start mark (C) first.
# That promise must hold whatever the user's own shell setup does, the setup
# itself must keep working, and a prompt that cannot keep the promise must not
# make it. Each case installs a user rc, seeds ~/.bash_history with the very
# command it will type, then types
#
#     [case-specific lines]  false && true  printf 'OUT%s\n' 42  <empty line>  exit
#
# and asserts on the mark stream, read as tokens M (P;CommandMarks=1), A, C,
# E (any 633;E) and O (the command's OUTPUT, OUT42 — its text has `OUT%s`):
#
#   - the case's expected run of tokens appears in order. For a case whose
#     prompts can keep the promise it is " M A C O E M A M A ": the capability
#     before A at every prompt; the printf's C before its output; its E
#     (history) after it; the empty line gets no C and no E; the next prompt
#     again M then A;
#   - at most the case's number of M tokens: a prompt that cannot prove its C
#     (a readonly PS0, stderr that is not the terminal, no integration at all)
#     must not announce;
#   - no E is sent twice: a seeded history, an empty line, a PROMPT_COMMAND
#     entry or a history file another shell appended to never re-sends one; the
#     printf has its one E wherever the expected run has one;
#   - the shell survived to `exit` (the output is there at all), and reported
#     no error (`unbound variable` under `set -u`, a `readonly variable`, …).
#
# Every shell runs in a pinned environment (`env -i`, plus what a runner pane
# sets), so the caller's — a runner pane exports QONTINUI_INTEGRATION and
# QONTINUI_RUNNER_TERMINAL — cannot change the result.
#
# Run: bash src-tauri/resources/shell-integration-marks-test.sh
# Exit 0 = pass (or SKIP without util-linux script(1)), 1 = a case failed.
# Driven from CI by src-tauri/src/session/spawn_prompt.rs
# `script_tests::bash_integration_marks_every_typed_command`, whose budget is
# sized from the number of `run_case` lines below and the per-case bound.
set -u

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
integ="${SHELL_INTEGRATION_BASH:-${here}/shell-integration.bash}"

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
trap 'rm -rf "${tmp}"' EXIT
esc=$'\033'
bel=$'\007'
typed="printf 'OUT%s\\n' 42"
failures=0
# The run every prompt that can keep the promise produces.
marks=" M A C O E M A M A "

# run_case <name> <expected run> <most M tokens, or any> <bashrc body> [lines typed first...]
run_case() {
    local name="$1" want="$2" max_m="$3" rc="$4"
    shift 4
    local home="${tmp}/${name}"
    mkdir -p "${home}"
    printf '%s\n' "${rc}" >"${home}/.bashrc"
    # The command this case types is already the newest history entry.
    printf '%s\n' "${typed}" >"${home}/.bash_history"
    # One line at a time, so each is read at its own prompt. The environment
    # is a runner pane's, and nothing of the caller's.
    local out
    out="$(
        {
            for line in "$@" "false && true" "${typed}" "" "exit"; do
                printf '%s\n' "${line}"
                sleep 0.3
            done
        } | env -i PATH="${PATH}" HOME="${home}" TERM=dumb QONTINUI_RUNNER_TERMINAL=1 \
            timeout -k 2 20 script -q -e -c "bash --rcfile '${integ}' -i" /dev/null 2>&1
    )"
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
        local m_count
        m_count="$(printf '%s' " ${seq} " | grep -o ' M ' | wc -l)"
        if [ "${m_count}" -gt "${max_m}" ]; then
            why="${why} P;CommandMarks=1 was announced ${m_count} times, at most ${max_m} allowed;"
        fi
    fi
    # The user's shell must not see the integration fail: no shell error at
    # all (an unbound variable under `set -u` aborts the rest of the rc).
    case "${out}" in
        *"unbound variable"* | *"readonly variable"* | *"syntax error"* | *"command not found"*)
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
    local e_count want_e
    e_count="$(printf '%s' "${out}" | grep -ao -e "${esc}]633;E;printf 'OUT%s" | wc -l)"
    case "${want}" in
        *"O E"*) want_e=1 ;;
        *) want_e=0 ;;
    esac
    if [ "${e_count}" -ne "${want_e}" ]; then
        why="${why} the printf's E was sent ${e_count} times, not ${want_e};"
    fi

    if [ -n "${why}" ]; then
        echo "FAIL ${name}:${why} (transcript: ${home}/transcript)"
        failures=$((failures + 1))
        # Keep the transcripts for the reader.
        trap - EXIT
    else
        echo "ok   ${name}"
    fi
}

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
# bash writes PS0 to stderr: once stderr is not the terminal, the C never
# reaches the pane, so only the prompt before the redirect announces.
run_case stderr-not-a-terminal " A O E A A " 1 'PS1="$ "' 'exec 2>/dev/null'
# A shared history (`history -a; history -c; history -r` every prompt) that
# another shell appended to: the reload renumbers the list, and the empty line
# after it must not re-send the previous line's E.
run_case shared-history "${marks}" any 'PS1="$ "; PROMPT_COMMAND="history -a; history -c; history -r"' \
    'echo from-other-session >> ~/.bash_history' ''

if [ "${failures}" -ne 0 ]; then
    echo "${failures} case(s) failed"
    exit 1
fi
echo "all cases passed"
