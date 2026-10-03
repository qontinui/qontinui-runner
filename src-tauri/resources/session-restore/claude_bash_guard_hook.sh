#!/usr/bin/env bash
# qontinui command-safety rewrite — Claude `PreToolUse` HOOK on `Bash`.
# Plan `2026-10-03-runner-sessions-stop-on-builtin-command-safety-prompts`,
# Phase 2.
#
# Delivered to Claude Code ADDITIVELY via the runner-owned `--settings`
# carrier, beside the SessionStart / PreCompact / Stop hooks (nothing is ever
# written to the user's `~/.claude/settings.json`). Its REGISTRATION is gated
# on the tenant's `command_safety_rewrite` fleet-policy dial (default on): a
# tenant that turned it off gets a carrier with no `PreToolUse` key at all.
# See `session::claude_hook`.
#
# WHY IT EXISTS. Claude Code runs built-in command-safety checks that survive
# `--permission-mode bypassPermissions` and stop the session for operator
# approval — observed verbatim:
#   Dangerous rm operation on possibly-empty variable path: $f in `rm -f $f`
# No allow rule silences that class, and none should: the check is right that
# the command is fragile. This hook answers a command it RECOGNISES as one the
# check will stop on with `permissionDecision: "deny"` and a reason that says
# how to rewrite it, so the agent rewrites and retries and the operator sees
# nothing. Probed on Claude Code 2.1.288: the deny pre-empts the built-in
# prompt in bypass, default and auto modes and inside a subagent.
#
# INVARIANTS
#   - NEVER `allow`. Only `deny`, and only on a positive rule match. Silencing
#     the built-in check would remove a real safety net.
#   - FAIL OPEN: empty / non-JSON stdin, another tool, no match, anything
#     unexpected -> exit 0 with NO output, so Claude behaves exactly as it
#     would without this hook (it may still prompt).
#   - ZERO CHILD PROCESSES on every path: no `cat`, `grep`, `jq`, `python`,
#     `sed`. This fires on Bash calls, and process creation on this fleet's
#     Windows/MSYS boxes costs 0.5-2.3 s per spawn. Builtins only: `read`,
#     `[[ =~ ]]`, parameter expansion, `printf`. The registration also carries
#     an `if: "Bash(*rm *)"` filter, so Claude does not start this script at
#     all for a command with no `rm` in it.
#
# NO JSON PARSER, ON PURPOSE. A `PreToolUse` payload nests `tool_input`, which
# every interpreter-free JSON rung in the sibling hooks declines. Instead one
# builtin regex lifts the `command` STRING VALUE out of the payload — still
# JSON-ESCAPED: a `"` in the command appears as `\"`, a backslash as `\\`, a
# newline as `\n` — and the rules see that and nothing else, so text in
# `description` (or any other field) can never draw a deny. Before the rules
# run, every shell-QUOTED segment of it is blanked: a separator, a `\n` or an
# `rm` inside `"..."` or `'...'` is data, not a command start. A rule that fails
# to match is a false negative, which costs exactly what today costs (one
# prompt); a false positive costs the agent one rewrite turn.
set -u

# ── The rule table ───────────────────────────────────────────────────────────
#
# One row per OBSERVED built-in prompt, never speculatively. Row shape mirrors
# the fleet's `command-traps.json` (`pattern`, `message`, and `observed_prompt`
# in place of `source_finding`):
#
#   rule_pattern[i]          bash ERE, matched with `[[ $payload =~ $re ]]`
#                            against the raw payload. Same dialect rules as
#                            command-traps.json's `_note`: no lazy
#                            quantifiers, and no escape INSIDE a bracket
#                            expression (a `\` inside `[...]` is a literal
#                            backslash, which is what the rows below rely on).
#                            Matched against the quote-blanked command text,
#                            not the payload, so `^` is the command's start.
#                            The first `$name` / `${name` in the matched text
#                            is the variable named in the reason.
#   rule_message[i]          the reason, ALREADY JSON-escaped (it is printed
#                            into the envelope verbatim). Constant and
#                            generic: `@VAR@` becomes `$<name>` (or "a $(...)
#                            expansion"), `@NAME@` becomes `<name>` (or
#                            `NAME`). It must never mention a variable the
#                            agent's command does not have — a probe saw an
#                            agent refuse a rewrite whose reason did.
#   rule_observed_prompt[i]  the built-in prompt text this row pre-empts,
#                            verbatim. Documentation, and the test fixture's
#                            anchor.
#
# Adding a rule: observe the built-in prompt first, quote it in a new row, and
# add its matching and non-matching fixtures to the guard tests in
# `session/spawn_prompt.rs`.
#
# ROW 0 — `rm` on a path built from an UNQUOTED variable expansion.
#   Matches `rm` (any flags, any earlier operands) at COMMAND-START position —
#   the start of the command string, or right after `;` `&` `|` `(` `{`, an
#   escaped newline, or ` then` / ` do` / ` else`, all OUTSIDE quotes — whose
#   operand contains an unquoted `$name`, `${name` or `$(` on the same line
#   before any separator, redirect, `#` or `)`.
#   Boundary decisions, each pinned by a test:
#     - `echo rm $x` does not match (rm is an argument, not a command).
#     - `rm -f "$f"` does NOT match. Phase 0 proved the built-in prompt only
#       for the UNQUOTED form, and quoted segments are blanked before matching.
#       Narrow by design (plan D2): a missed quoted case costs one prompt, as
#       today. The same blanking makes `git commit -m "fix; rm $x"` and
#       `printf 'a\nrm $x'` non-matches.
#     - `rm -f ${x:?}` does NOT match: `${name:?…}` aborts on an empty value,
#       and it is the very rewrite the reason recommends.
#     - `rm -f /tmp/$x` matches: the path still collapses when `$x` is empty.
#     - A command carrying a heredoc (`<<`) is not judged at all — the hook
#       cannot tell heredoc prose from command text without parsing — so
#       `rm $x` written inside a heredoc never draws a deny.
#     - `rm -f "/literal/path"` and `rm -rf build/` carry no expansion.
rule_pattern=(
  '(^|;|&|\||\(|\{|\\n| then| do| else) *rm +[^;&|<>$'"'"'#)\]*\$(\{[A-Za-z_][A-Za-z0-9_]*([^:A-Za-z0-9_]|:[^?]|$)|[A-Za-z_]|\()'
)
rule_message=(
  'Not run: this command passes rm a path built from @VAR@, and Claude Code stops that for operator approval (\"Dangerous rm operation on possibly-empty variable path\") because an empty value collapses the path toward the filesystem root. Rewrite the command so no approval is needed, then run it again. Best: drop the rm if it only cleans up something the command does not need. Otherwise write to a literal path or one made by mktemp, or refuse an empty value before deleting: rm -f \"${@NAME@:?}\" or [ -n \"$@NAME@\" ] && rm -f \"$@NAME@\".'
)
rule_observed_prompt=(
  'Dangerous rm operation on possibly-empty variable path'
)

# Drain stdin with a BUILTIN, not `cat` (see `claude_stop_hook.sh`): `read -d ''`
# slurps to EOF; `-t 1` bounds an event that attaches no stdin.
payload=""
IFS= read -r -t 1 -d '' payload || true
[ -z "$payload" ] && exit 0

# Only `Bash`. `"tool_name":"BashOutput"` does not contain `"tool_name":"Bash"`
# (the closing quote is part of the needle).
case "$payload" in
  *'"tool_name":"Bash"'* | *'"tool_name": "Bash"'*) ;;
  *) exit 0 ;;
esac

# The `command` string value, still JSON-escaped: a run of non-quote,
# non-backslash characters or `\<any>` escapes up to the first unescaped `"`.
# A `"command"` key inside another field's TEXT is spelled `\"command\"` in the
# payload, so it cannot match here. No command value ⇒ nothing to judge.
cmd_re='"command": ?"(([^"\]|\\.)*)"'
[[ $payload =~ $cmd_re ]] || exit 0
cmd="${BASH_REMATCH[1]}"

# Heredoc: not judged (see ROW 0).
case "$cmd" in
  *'<<'*) exit 0 ;;
esac

# Blank every shell-quoted segment, double-quoted first (a `'` inside "..." is
# data), then single-quoted. In the JSON-escaped text a shell `"` is `\"`, a
# shell backslash is `\\`, so a double-quoted segment is `\"`, then tokens that
# are a plain character, a JSON escape other than `\"` / `\\`, or a shell
# backslash followed by any one token, then `\"`. Each pass removes at least
# two characters, so both loops terminate. An unbalanced quote is simply left
# in place (the rules then see more text, never less).
dq_re='\\"([^\]|\\[^"\]|\\\\([^\]|\\.))*\\"'
while [[ $cmd =~ $dq_re ]]; do
  cmd="${cmd/"${BASH_REMATCH[0]}"/Q}"
done
sq_re="'[^']*'"
while [[ $cmd =~ $sq_re ]]; do
  cmd="${cmd/"${BASH_REMATCH[0]}"/Q}"
done

name_re='\$\{?([A-Za-z_][A-Za-z0-9_]*)'
i=0
while [ "$i" -lt "${#rule_pattern[@]}" ]; do
  re="${rule_pattern[$i]}"
  if [[ $cmd =~ $re ]]; then
    matched="${BASH_REMATCH[0]}"
    if [[ $matched =~ $name_re ]]; then
      name="${BASH_REMATCH[1]}"
      var="\$$name"
    else
      var="a \$(...) expansion"
      name="NAME"
    fi
    msg="${rule_message[$i]}"
    msg="${msg//@VAR@/$var}"
    msg="${msg//@NAME@/$name}"
    printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"%s"}}\n' "$msg"
    exit 0
  fi
  i=$((i + 1))
done
exit 0
