#!/bin/bash
# Shared helper — SOURCE this, do not execute it.
#
# ONE decision record, ONE format, ONE durable location, for every guard in this
# component. Plan 2026-08-20-hooks-and-skills-are-a-component-system-nobody-owns,
# Phase 5.
#
# Format (TAB-separated, one line per decision):
#   <epoch.microseconds>\t<pid>\t<guard>\t<verdict>\t<tag>\t<command>
#
# Path: $QONTINUI_GUARD_DECISION_LOG, else <log-dir>/guard-decisions.log, where
# <log-dir> is $QONTINUI_LOG_DIR, else ~/.qontinui/logs.
#
# ── What this replaces, and why it is a correctness fix rather than tidying ───
# Before this, guard decisions were spread over four files in three grammars,
# two of them under /tmp. That was the stated Phase 5 problem. The unstated one
# was worse, and it is the actual reason this file exists:
#
#   git-guard.sh did not APPEND a verdict. It appended `ALLOW` and then
#   REWROTE it in place — `sed -i '$ s/ALLOW/BLOCK/'` — from 16 separate sites.
#   sccache-backend-guard.sh did the same from 2 more.
#
# `sed -i '$ ...'` addresses whichever line is LAST AT THAT INSTANT. These logs
# are shared by every concurrent session on the box (~9 of them here), so under
# any interleaving the rewrite can land on a PEER's record: one session's block
# recorded against another session's command, with nothing in the file to show
# it happened. An audit log that can silently misattribute is worse than no
# audit log, because it is trusted.
#
# So the write pattern is what changed. Every exit path calls `guard_decide`
# exactly once, immediately before it exits, and NOTHING is ever rewritten.
# Removing those 18 `sed -i` calls is the point of the change, not a side
# effect of moving the path.
#
# ── Cost discipline: this is the hottest path in the component ───────────────
# The Bash guards run on every `git`/`rm`/`cargo` tool call in every session, and
# several match the same call. So, exactly as `lib/hook-latency.sh` argues for
# its own breadcrumb, this spawns NOTHING:
#
#   * `$EPOCHREALTIME` is a bash BUILTIN VARIABLE (bash >= 5.0), not `date`.
#     The old records used `$(date -Iseconds)`, which FORKS — git-guard.sh paid
#     that fork on every git, rm and cargo call on this machine. Dropping it is
#     a straight saving on the hot path, and the reason the timestamp spelling
#     changed rather than being carried over.
#   * `printf` and `>>` are builtins; the parameter mangling below is pure bash
#     parameter expansion, no `sed`/`tr`.
#   * There is deliberately NO `mkdir -p "$(dirname …)"` — a command
#     substitution plus a mkdir is two spawns, which would cost more than
#     everything the rest of this saves. The redirect simply fails and `|| true`
#     swallows it when the directory is absent. SessionStart creates it (see
#     session-id-stamp.sh), so the degraded case is "no breadcrumb", never a
#     slow or broken turn.
#
# ── Why TAB and not the old space-separated shape ────────────────────────────
# The last field is a COMMAND, which contains spaces. `lib/hook-latency.sh`'s
# space-separated 4-field format cannot carry one, and its reader
# (`analyze-hook-latency.py`) splits on whitespace and requires exactly four
# fields — so a command could never have gone in that log. TAB is what
# `lib/guard-shadow.sh` already uses for the same reason, and this is that
# grammar generalised rather than a third invention. Newlines and tabs inside
# the command are collapsed to spaces so ONE decision is always ONE record.
#
# ── Rotation is NOT here ─────────────────────────────────────────────────────
# Same reasoning as hook-latency.sh: bash has no spawnless way to read a file's
# size, so a cap check on this path would reintroduce the fork this file exists
# to avoid. `lib/log-rotate.sh` does it at SessionStart, where a spawn is
# already affordable and fires once per session rather than once per call.
#
# ── This log records DECISIONS. Two sibling streams record other things ──────
# Deliberately three files, not one. Phase 5 asked for "one durable location,
# one format"; taken as one FILE it would force every reader to parse and
# discard the other two subjects:
#
#   guard-decisions.log  what the guards allowed and blocked   (this file)
#   hook-latency.log     what each hook cost, per turn/call    (lib/hook-latency.sh)
#   guard-shadow.log     where the normalizer disagrees with the live gate
#                                                              (lib/guard-shadow.sh)
#
# One location, one format family, three subjects. `guard-shadow.log` is already
# self-bounding (it records only disagreements) and `hook-latency.log` is read by
# a committed analyzer that requires its own 4-field shape — merging either into
# this file would break a working reader to satisfy a word.

QONTINUI_GUARD_DECISION_LOG="${QONTINUI_GUARD_DECISION_LOG:-${QONTINUI_LOG_DIR:-${HOME:-}/.qontinui/logs}/guard-decisions.log}"

# guard_decide <guard> <verdict> <tag> [command]
#
#   <guard>   the guard's own name, e.g. git-guard, cargo-guard-hook.
#   <verdict> one of: allow | block | warn | skip | unknown.
#             `skip` is a guard that declined to judge (prefilter said the call
#             is none of its business). `unknown` is a guard that could not
#             establish what it needed — a DEGRADED guard, which is the state
#             this component keeps mistaking for a working one, so it gets its
#             own verdict rather than being folded into `allow`.
#   <tag>     a short stable reason token, no spaces, e.g. reset-hard,
#             stash-drop, lib-unreadable. This is the field you grep and count.
#   [command] the tool call being judged. Optional — omit for decisions that are
#             not about a command.
#
# Never fails, never blocks, never writes a partial record.
guard_decide() {
  local guard="${1:-unknown}"
  local verdict="${2:-unknown}"
  local tag="${3:-none}"
  local cmd="${4-}"

  # Collapse the field separators so one decision is always one parseable
  # record. Pure parameter expansion — no `tr`, which would fork on the hot
  # path. Same treatment guard-shadow.sh applies for the same reason.
  cmd=${cmd//$'\n'/ }
  cmd=${cmd//$'\r'/ }
  cmd=${cmd//$'\t'/ }

  # The first three fields are ours and are space-free by construction; scrub
  # them anyway. A guard passing a tag with a tab in it would otherwise shift
  # every later field left, and the reader would mis-attribute silently — the
  # same class of quiet corruption this file was written to remove.
  guard=${guard//[$'\t\n\r']/}
  verdict=${verdict//[$'\t\n\r']/}
  tag=${tag//[$'\t\n\r']/}

  # ⚠️ REDIRECTION ORDER IS LOAD-BEARING: `2>/dev/null` comes BEFORE `>>`.
  #
  # bash applies redirections left to right. Written the other way round —
  # `>> "$LOG" 2>/dev/null`, which is the obvious spelling and the one
  # `lib/hook-latency.sh` uses — the append is attempted FIRST, and when the
  # directory does not exist bash writes `No such file or directory` to the
  # still-real stderr before `2>/dev/null` is ever applied. `|| true` does not
  # help: it swallows the exit STATUS, not the message.
  #
  # That is not theoretical. Caught 2026-08-24 by `wip-custody-test.sh`, which
  # asserts `git-guard.sh` stays silent on `git stash create` and instead saw
  # this library's redirect error on the guard's stderr — a guard made noisy by
  # its own audit log, on any machine whose log directory SessionStart has not
  # created yet. Putting stderr on /dev/null first means the failed append is
  # reported into /dev/null, which is the intent everywhere in this component:
  # no breadcrumb, never a broken or noisy hook.
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "${EPOCHREALTIME:-0}" "$$" "$guard" "$verdict" "$tag" "$cmd" \
    2>/dev/null >> "$QONTINUI_GUARD_DECISION_LOG" || true
}
