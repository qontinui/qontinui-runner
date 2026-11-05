#!/usr/bin/env bash
# shell-integration-marks-test.sh — the bash integration's prompt-state marks,
# observed on a real pty.
#
# The runner replaces a terminal pane ("Move them") only when its shell's last
# OSC 633 mark is a prompt (A) AND that prompt announced `P;CommandMarks=1`,
# i.e. that the next typed command line gets a command-start mark (C) first.
# That promise must hold whatever the user's own shell setup does, and the
# setup itself must keep working. Each case below installs a user rc (no
# PROMPT_COMMAND, string form, string ending in `;`, array form, one that
# rewrites PS1 every prompt, `set -u`, `set -e`, a PS0 reassigned at runtime),
# seeds ~/.bash_history with the very command it will type, then types
#
#     [case-specific lines]  false && true  printf 'OUT%s\n' 42  <empty line>  exit
#
# and asserts on the mark stream, read as tokens M (P;CommandMarks=1), A, C,
# E (any 633;E) and O (the command's OUTPUT, OUT42 — its text has `OUT%s`):
#
#   - " M A C O E M A M A " in order: the capability before A at every prompt;
#     the printf's C before its output; its E (history) after it, once; the
#     empty line gets no C and no E; the next prompt again M then A;
#   - `printf` has exactly one E — a seeded history, an empty line or a
#     PROMPT_COMMAND entry never re-sends it;
#   - the shell survived to `exit` (the output is there at all), and reported
#     no error (`unbound variable` under `set -u`, a `readonly variable`, …).
#
# Run: bash src-tauri/resources/shell-integration-marks-test.sh
# Exit 0 = pass (or SKIP without util-linux script(1)), 1 = a case failed.
# Driven from CI by src-tauri/src/session/spawn_prompt.rs
# `script_tests::bash_integration_marks_every_typed_command`.
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

# run_case <name> <bashrc body> [lines typed first...]
run_case() {
    local name="$1" rc="$2"
    shift 2
    local home="${tmp}/${name}"
    mkdir -p "${home}"
    printf '%s\n' "${rc}" >"${home}/.bashrc"
    # The command this case types is already the newest history entry.
    printf '%s\n' "${typed}" >"${home}/.bash_history"
    # One line at a time, so each is read at its own prompt.
    local out
    out="$(
        {
            for line in "$@" "false && true" "${typed}" "" "exit"; do
                printf '%s\n' "${line}"
                sleep 0.3
            done
        } | HOME="${home}" TERM=dumb timeout -k 2 30 \
            script -q -e -c "bash --rcfile '${integ}' -i" /dev/null 2>&1
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
        *" M A C O E M A M A "*) ;;
        *) why="${why} expected ' M A C O E M A M A ' in the mark stream, got '${seq}';" ;;
    esac
    # The user's shell must not see the integration fail: no shell error at
    # all (an unbound variable under `set -u` aborts the rest of the rc).
    case "${out}" in
        *"unbound variable"* | *"readonly variable"* | *"syntax error"* | *"command not found"*)
            why="${why} the shell reported an error;"
            ;;
    esac
    local e_count
    e_count="$(printf '%s' "${out}" | grep -ao -e "${esc}]633;E;printf 'OUT%s" | wc -l)"
    if [ "${e_count}" -ne 1 ]; then
        why="${why} the printf's E was sent ${e_count} times, not once;"
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

run_case no-prompt-command 'PS1="$ "'
run_case string-prompt-command 'PS1="$ "; PROMPT_COMMAND="history -a"'
run_case string-ending-in-semicolon 'PS1="$ "; PROMPT_COMMAND="history -a; true;"'
run_case array-prompt-command 'PS1="$ "; PROMPT_COMMAND=(true "history -a")'
run_case rewrites-ps1-every-prompt 'PS1="$ "; PROMPT_COMMAND='"'"'history -a; PS1="\$ "'"'"
run_case set-u 'PS1="$ "; set -u; PROMPT_COMMAND="history -a"'
run_case set-e 'PS1="$ "; set -e; PROMPT_COMMAND="history -a"'
# PS0 reassigned at runtime (a re-sourced rc does this): the next prompt must
# re-prefix our C before it may announce the capability.
run_case ps0-reassigned 'PS1="$ "; PROMPT_COMMAND="history -a"' 'PS0=""'

if [ "${failures}" -ne 0 ]; then
    echo "${failures} case(s) failed"
    exit 1
fi
echo "all cases passed"
