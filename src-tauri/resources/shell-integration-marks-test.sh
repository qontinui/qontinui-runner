#!/usr/bin/env bash
# shell-integration-marks-test.sh — the bash integration's prompt-state marks,
# observed on a real pty.
#
# The runner replaces a terminal pane ("Move them") only when its shell's last
# OSC 633 mark is a prompt (A) AND the shell announced `P;CommandMarks=1`, i.e.
# that every typed command line gets a command-start mark (C) first. That
# promise must hold whatever the user's own shell setup does, so each case
# below installs a user PROMPT_COMMAND (none, string form, array form, one
# ending in `;`, one that rewrites PS1 every prompt), types a command whose
# OUTPUT differs from its text, and asserts on the mark stream:
#
#   - P;CommandMarks=1 is announced (bash >= 4.4 — the PS0 path);
#   - the typed command's output is preceded by its C, with no A between them,
#     and followed by an A (the next prompt);
#   - the command's E (its text, for the webview's command history) is sent
#     exactly once.
#
# Run: bash src-tauri/resources/shell-integration-marks-test.sh
# Exit 0 = pass (or SKIP without util-linux script(1)), 1 = a case failed.
# Driven from CI by src-tauri/src/session/spawn_prompt.rs
# `script_tests::bash_integration_marks_every_typed_command`.
set -u

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
integ="${here}/shell-integration.bash"

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
failures=0

# run_case <name> <bashrc body>
run_case() {
    local name="$1" rc="$2"
    local home="${tmp}/${name}"
    mkdir -p "${home}"
    printf '%s\n' "${rc}" >"${home}/.bashrc"
    # The typed text contains `OUT%s`; only the command's OUTPUT contains OUT42.
    local out
    out="$(printf '%s\n' "printf 'OUT%s\\n' 42" "exit" |
        HOME="${home}" TERM=dumb timeout -k 2 20 \
            script -q -e -c "bash --rcfile '${integ}' -i" /dev/null 2>&1)"
    printf '%s' "${out}" >"${home}/transcript"

    # The marks and the output token, in stream order, one word each.
    local seq
    seq="$(printf '%s' "${out}" |
        grep -ao -e "${esc}]633;A${bel}" -e "${esc}]633;C${bel}" -e "OUT42" |
        sed -e "s/^${esc}]633;A${bel}\$/A/" -e "s/^${esc}]633;C${bel}\$/C/" -e 's/^OUT42$/O/' |
        tr '\n' ' ')"

    local why=""
    case "${out}" in
        *"${esc}]633;P;CommandMarks=1${bel}"*) ;;
        *) why="${why} no P;CommandMarks=1;" ;;
    esac
    case " ${seq} " in
        *" A C O A "*) ;;
        *) why="${why} expected 'A C O A' in the mark stream, got '${seq}';" ;;
    esac
    local e_count
    e_count="$(printf '%s' "${out}" | grep -ao -e "${esc}]633;E;printf 'OUT%s" | wc -l)"
    if [ "${e_count}" -ne 1 ]; then
        why="${why} the command's E was sent ${e_count} times, not once;"
    fi

    if [ -n "${why}" ]; then
        echo "FAIL ${name}:${why} (transcript: ${home}/transcript)"
        failures=$((failures + 1))
        # Keep the transcript for the reader.
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

if [ "${failures}" -ne 0 ]; then
    echo "${failures} case(s) failed"
    exit 1
fi
echo "all cases passed"
